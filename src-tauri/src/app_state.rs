use crate::automation::cancellation::{CancellationRegistry, CancellationToken};
use crate::browser::session_registry::SessionRegistry;
use crate::errors::AppResult;
use crate::storage::db::Db;
use std::path::PathBuf;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct AppState {
    db: Db,
    data_dir: Arc<PathBuf>,
    sessions: SessionRegistry,
    cancellations: CancellationRegistry,
    batch_semaphores: Arc<Mutex<HashMap<String, Arc<Semaphore>>>>,
}

impl AppState {
    pub fn initialize(data_dir: PathBuf) -> AppResult<Self> {
        let db = Db::initialize(data_dir.clone())?;
        crate::browser::recovery::recover_startup_state(&db)?;

        Ok(Self {
            db,
            data_dir: Arc::new(data_dir),
            sessions: SessionRegistry::default(),
            cancellations: CancellationRegistry::default(),
            batch_semaphores: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub fn data_dir(&self) -> &PathBuf {
        &self.data_dir
    }

    pub fn sessions(&self) -> &SessionRegistry {
        &self.sessions
    }

    /// Return a shared concurrency gate for a batch. Re-entering the scheduler
    /// (for example, when retrying a run) must not create a second independent limit.
    pub fn batch_semaphore(&self, batch_id: &str, permits: usize) -> Arc<Semaphore> {
        let mut gates = self.batch_semaphores.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        gates
            .entry(batch_id.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(permits.max(1))))
            .clone()
    }

    pub fn register_cancellation(&self, run_id: &str) -> CancellationToken {
        self.cancellations.register(run_id)
    }

    pub fn cancel_run(&self, run_id: &str) -> bool {
        self.cancellations.cancel(run_id)
    }

    pub fn remove_cancellation(&self, run_id: &str) {
        self.cancellations.remove(run_id);
    }
}
