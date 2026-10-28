use crate::app_state::AppState;
use crate::automation::{deno_runtime, task_runner};
use crate::domain::run::{RunBatch, RunTaskInput, TaskRunStatus};
use crate::domain::task::{AutomationTask, SaveTaskInput, ValidateTaskScriptResult};
use crate::errors::{AppError, AppResult};
use crate::storage::{legacy_cleanup, run_repo, task_repo};
use tauri::{AppHandle, Emitter, State};

#[tauri::command]
pub fn list_tasks(state: State<'_, AppState>) -> AppResult<Vec<AutomationTask>> {
    task_repo::list(state.db())
}

#[tauri::command]
pub fn save_task(state: State<'_, AppState>, input: SaveTaskInput) -> AppResult<AutomationTask> {
    let validation = validate_script(&input.script);
    if !validation.valid {
        return Err(AppError::new(
            "script_compile_error",
            validation
                .errors
                .first()
                .cloned()
                .unwrap_or_else(|| "Script validation failed".to_string()),
        ));
    }
    task_repo::save(state.db(), input)
}

#[tauri::command]
pub fn delete_task(state: State<'_, AppState>, id: String) -> AppResult<()> {
    task_repo::get(state.db(), &id)?;
    // Check the database directly instead of the UI's capped recent-runs list;
    // an older active run must still prevent its task from being deleted.
    if run_repo::has_active_runs_for_task(state.db(), &id)? {
        return Err(AppError::new(
            "task_has_active_runs",
            "Cannot delete a task while it has queued or active runs. Cancel or finish those runs first.",
        ));
    }
    let artifact_dirs = run_repo::delete_runs_for_task(state.db(), &id)?;
    legacy_cleanup::delete_ai_conversations_for_task(state.db(), &id)?;
    legacy_cleanup::clear_agent_task_reference(state.db(), &id)?;
    task_repo::hard_delete(state.db(), &id)?;
    cleanup_artifact_dirs(&state, artifact_dirs);
    Ok(())
}

#[tauri::command]
pub fn validate_task_script(script: String) -> ValidateTaskScriptResult {
    validate_script(&script)
}

#[tauri::command]
pub async fn run_task(
    app: AppHandle,
    state: State<'_, AppState>,
    input: RunTaskInput,
) -> AppResult<RunBatch> {
    let task = task_repo::get(state.db(), &input.task_id)?;
    let mut options = input.options.clone().unwrap_or_default();
    options.max_concurrency = options.max_concurrency.clamp(1, 32);
    let mut input = input;
    input.options = Some(options.clone());
    let batch = run_repo::create_queued_batch(state.db(), input, task.timeout_sec)?;
    let _ = app.emit("batch_progress_changed", &batch);
    spawn_batch_runs(app, state.inner().clone(), batch.clone(), task, options);
    Ok(batch)
}

#[tauri::command]
pub fn cancel_run(app: AppHandle, state: State<'_, AppState>, run_id: String) -> AppResult<()> {
    let run = run_repo::get_run(state.db(), &run_id)?;
    if matches!(
        run.status,
        TaskRunStatus::Succeeded
            | TaskRunStatus::Failed
            | TaskRunStatus::Cancelled
            | TaskRunStatus::TimedOut
            | TaskRunStatus::Interrupted
            | TaskRunStatus::CancelRequested
    ) {
        return Ok(());
    }
    let status = if state.cancel_run(&run_id) {
        TaskRunStatus::CancelRequested
    } else {
        TaskRunStatus::Cancelled
    };
    if set_cancel_status_if_active(&state, &run_id, status.clone())? {
        emit_run_status(&app, &run_id, status);
        emit_batch_progress(&app, &state, &run)?;
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_batch(app: AppHandle, state: State<'_, AppState>, batch_id: String) -> AppResult<()> {
    let mut first_error = None;
    for run in run_repo::list_runs_by_batch(state.db(), &batch_id)? {
        if matches!(
            run.status,
            TaskRunStatus::Succeeded
                | TaskRunStatus::Failed
                | TaskRunStatus::Cancelled
                | TaskRunStatus::CancelRequested
                | TaskRunStatus::TimedOut
                | TaskRunStatus::Interrupted
        ) {
            continue;
        }
        let status = if state.cancel_run(&run.id) {
            TaskRunStatus::CancelRequested
        } else {
            TaskRunStatus::Cancelled
        };
        match set_cancel_status_if_active(&state, &run.id, status.clone()) {
            Ok(true) => emit_run_status(&app, &run.id, status),
            Ok(false) => {}
            Err(err) => {
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
        }
    }
    if let Ok(batch) = run_repo::get_batch(state.db(), &batch_id) {
        let _ = app.emit("batch_progress_changed", &batch);
    }
    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

#[tauri::command]
pub fn retry_run(app: AppHandle, state: State<'_, AppState>, run_id: String) -> AppResult<()> {
    // Resolve all scheduler dependencies before inserting a new queued attempt.
    // Otherwise a task/batch lookup error after create_retry_run succeeds could
    // leave a queued run with no scheduler responsible for executing it.
    let source = run_repo::get_run(state.db(), &run_id)?;
    let task = task_repo::get(state.db(), &source.task_id)?;
    let batch = match source.batch_id.as_deref() {
        Some(batch_id) => Some(run_repo::get_batch(state.db(), batch_id)?),
        None => None,
    };

    let retry = run_repo::create_retry_run(state.db(), &run_id)?;
    let batch = match batch {
        Some(previous) => {
            let refreshed = match run_repo::get_batch(state.db(), &previous.id) {
                Ok(batch) => batch,
                Err(err) => {
                    let _ = app.emit(
                        "diagnostic_warning",
                        format!("Retry {} was created but batch progress refresh failed: {}", retry.id, err.message),
                    );
                    previous
                }
            };
            Some(refreshed)
        }
        None => None,
    };
    let mut options = batch
        .as_ref()
        .map(|batch| batch.options.clone())
        .unwrap_or_default();
    options.max_concurrency = options.max_concurrency.clamp(1, 32);
    if let Some(batch) = batch {
        // create_retry_run refreshes persisted counters; fetch the batch again so
        // the UI receives current queued/running/finished counts, not a stale snapshot.
        let _ = app.emit("batch_progress_changed", &batch);
        // Re-enter the batch scheduler so retries obey the same concurrency limit
        // and stop-on-first-error policy as the original batch.
        spawn_batch_runs(app, state.inner().clone(), batch, task, options);
    } else {
        // Legacy runs without a batch retain the single-run retry behavior.
        tauri::async_runtime::spawn(execute_one_run(
            app,
            state.inner().clone(),
            retry,
            task,
            options,
        ));
    }
    Ok(())
}

pub fn resume_queued_batches(app: AppHandle, state: AppState) -> AppResult<usize> {
    let batches = run_repo::list_batches_with_queued_runs(state.db())?;
    let total = batches.len();
    for batch in batches {
        let task = match task_repo::get(state.db(), &batch.task_id) {
            Ok(task) => task,
            Err(err) => {
                if err.code == "task_not_found" {
                    if let Ok(runs) = run_repo::list_runs_by_batch(state.db(), &batch.id) {
                        for run in runs {
                            if run.status == TaskRunStatus::Queued {
                                if let Err(finish_err) = run_repo::mark_finished(
                                    state.db(),
                                    &run.id,
                                    TaskRunStatus::Failed,
                                    Some("task_not_found"),
                                    Some("Task is missing; queued run cannot be resumed"),
                                ) {
                                    let _ = app.emit(
                                        "diagnostic_warning",
                                        format!("Failed to close queued run {}: {}", run.id, finish_err.message),
                                    );
                                } else {
                                    emit_run_status(&app, &run.id, TaskRunStatus::Failed);
                                }
                            }
                        }
                        if let Ok(updated_batch) = run_repo::get_batch(state.db(), &batch.id) {
                            let _ = app.emit("batch_progress_changed", &updated_batch);
                        }
                    }
                }
                let _ = app.emit(
                    "diagnostic_warning",
                    format!("Failed to recover batch {}: {}", batch.id, err.message),
                );
                continue;
            }
        };
        let options = batch.options.clone();
        spawn_batch_runs(app.clone(), state.clone(), batch, task, options);
    }
    Ok(total)
}

fn spawn_batch_runs(
    app: AppHandle,
    state: AppState,
    batch: RunBatch,
    task: AutomationTask,
    options: crate::domain::run::RunOptions,
) {
    tauri::async_runtime::spawn(async move {
        // SQLite can be briefly busy while another command updates run state.
        // Retry transient reads before abandoning this scheduler and leaving queued
        // runs waiting until the next application restart.
        let mut last_error = None;
        let mut loaded_runs = None;
        for attempt in 0..3 {
            match run_repo::list_runs_by_batch(state.db(), &batch.id) {
                Ok(runs) => {
                    loaded_runs = Some(runs);
                    break;
                }
                Err(err) => {
                    last_error = Some(err);
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_millis(200 * (attempt + 1))).await;
                    }
                }
            }
        }
        let runs = match loaded_runs {
            Some(runs) => runs,
            None => {
                let message = last_error
                    .map(|err| err.message)
                    .unwrap_or_else(|| "Unknown storage error".to_string());
                let _ = app.emit(
                    "diagnostic_warning",
                    format!("Failed to read batch runs after retries: {}", message),
                );
                return;
            }
        };

        let semaphore = state.batch_semaphore(
            &batch.id,
            if options.stop_on_first_error {
                1
            } else {
                options.max_concurrency.max(1) as usize
            },
        );

        if options.stop_on_first_error {
            // Keep the entire stop-on-first-error sequence exclusive even if a retry
            // starts another scheduler for this same batch.
            let Ok(_batch_permit) = semaphore.clone().acquire_owned().await else {
                return;
            };
            // A manual retry is a new attempt. Historical failures from lower
            // attempts must not block that retry, but must still stop queued first
            // attempts when stop-on-first-error is enabled.
            let first_queued_attempt = runs
                .iter()
                .filter(|run| run.status == TaskRunStatus::Queued)
                .map(|run| run.attempt)
                .min();
            let mut failed = first_queued_attempt.is_some_and(|attempt| {
                runs.iter().any(|run| {
                    run.attempt >= attempt
                        && matches!(
                            run.status,
                            TaskRunStatus::Failed
                                | TaskRunStatus::TimedOut
                                | TaskRunStatus::Interrupted
                        )
                })
            });
            for run in runs {
                if run.status != TaskRunStatus::Queued {
                    continue;
                }
                if failed {
                    if run_repo::mark_finished(
                        state.db(),
                        &run.id,
                        TaskRunStatus::Cancelled,
                        Some("stop_on_first_error"),
                        Some("Cancelled because an earlier run in this batch failed"),
                    )
                    .is_ok()
                    {
                        emit_run_status(&app, &run.id, TaskRunStatus::Cancelled);
                        let _ = emit_batch_progress(&app, &state, &run);
                    }
                    continue;
                }
                let result = execute_one_run(
                    app.clone(),
                    state.clone(),
                    run,
                    task.clone(),
                    options.clone(),
                )
                .await;
                // A user-requested cancellation is not a task failure and should
                // not trigger stop-on-first-error for the remaining queued runs.
                failed = match result {
                    Ok(()) => false,
                    Err(err) => err.code != "task_cancelled",
                };
            }
            return;
        }

        let mut handles = Vec::new();
        for run in runs {
            if run.status != TaskRunStatus::Queued {
                continue;
            }
            let Ok(permit) = semaphore.clone().acquire_owned().await else {
                continue;
            };
            let app = app.clone();
            let state = state.clone();
            let task = task.clone();
            let options = options.clone();
            handles.push(tauri::async_runtime::spawn(async move {
                let _permit = permit;
                let _ = execute_one_run(app, state, run, task, options).await;
            }));
        }
        for handle in handles {
            let _ = handle.await;
        }
    });
}

async fn execute_one_run(
    app: AppHandle,
    state: AppState,
    run: crate::domain::run::TaskRun,
    task: AutomationTask,
    options: crate::domain::run::RunOptions,
) -> AppResult<()> {
    let current = run_repo::get_run(state.db(), &run.id)?;
    if current.status != TaskRunStatus::Queued {
        return Ok(());
    }
    let token = state.register_cancellation(&run.id);
    let result = task_runner::execute_task_run(
        state.clone(),
        Some(app),
        current.clone(),
        task,
        options,
        token,
    )
    .await;
    state.remove_cancellation(&run.id);
    result
}

// A run can finish between the initial status read and the cancellation write.
// Treat that race as a no-op, but preserve genuine storage errors while the run
// is still in a cancellable state.
fn set_cancel_status_if_active(
    state: &AppState,
    run_id: &str,
    status: TaskRunStatus,
) -> AppResult<bool> {
    match run_repo::set_status(state.db(), run_id, status) {
        Ok(()) => Ok(true),
        Err(err) => match run_repo::get_run(state.db(), run_id) {
            Ok(current)
                if !matches!(
                    current.status,
                    TaskRunStatus::Queued
                        | TaskRunStatus::Starting
                        | TaskRunStatus::Running
                ) =>
            {
                Ok(false)
            }
            _ => Err(err),
        },
    }
}

fn emit_run_status(app: &AppHandle, run_id: &str, status: TaskRunStatus) {
    let _ = app.emit(
        "run_status_changed",
        serde_json::json!({
            "run_id": run_id,
            "status": status,
            "updated_at": chrono::Utc::now().to_rfc3339(),
        }),
    );
}

fn emit_batch_progress(
    app: &AppHandle,
    state: &AppState,
    run: &crate::domain::run::TaskRun,
) -> AppResult<()> {
    let Some(batch_id) = run.batch_id.as_deref() else {
        return Ok(());
    };
    let batch = run_repo::get_batch(state.db(), batch_id)?;
    let _ = app.emit("batch_progress_changed", &batch);
    Ok(())
}

fn cleanup_artifact_dirs(state: &AppState, artifact_dirs: Vec<String>) {
    for relative_dir in artifact_dirs {
        let path = state.data_dir().join(relative_dir);
        if path.exists() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn validate_script(script: &str) -> ValidateTaskScriptResult {
    let trimmed = script.trim();
    if trimmed.is_empty() {
        return ValidateTaskScriptResult {
            valid: false,
            errors: vec!["Script cannot be empty".to_string()],
            warnings: Vec::new(),
        };
    }

    if let Err(err) = deno_runtime::validate_script_surface(trimmed) {
        return ValidateTaskScriptResult {
            valid: false,
            errors: vec![err.message],
            warnings: deno_runtime::collect_script_warnings(trimmed),
        };
    }

    ValidateTaskScriptResult {
        valid: true,
        errors: Vec::new(),
        warnings: deno_runtime::collect_script_warnings(trimmed),
    }
}
