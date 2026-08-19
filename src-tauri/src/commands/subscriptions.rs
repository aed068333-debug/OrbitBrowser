use crate::app_state::AppState;
use crate::domain::environment::{BrowserKind, EnvironmentMode, SaveEnvironmentInput};
use crate::domain::proxy::{ProxyConfig, ProxyKind};
use crate::domain::subscription::{
    ApplySubscriptionNodeInput, CreateEnvironmentsFromNodesInput, CreateEnvironmentsFromNodesResult,
    LocalProxyEndpoint, ProbeSubscriptionNodesInput, ProbeSubscriptionNodesResult, ProxyCoreStatus,
    ProxySubscription, ProxySubscriptionNode, ProxySubscriptionSyncResult,
    SaveProxySubscriptionInput, SubscriptionNodeLatency, SubscriptionNodeProtocol,
};
use crate::errors::{AppError, AppResult};
use crate::proxy::{core_manager, subscription_parser};
use crate::storage::{environment_repo, settings_repo, subscription_repo};
use serde_json::{json, Value};
use std::net::ToSocketAddrs;
use std::time::{Duration, Instant};
use tauri::State;
use tokio::task::JoinSet;

fn proxy_core_override(db: &crate::storage::db::Db) -> Option<String> {
    if let Ok(value) = std::env::var("ORBIT_PROXY_CORE") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    settings_repo::get(db)
        .ok()
        .and_then(|settings| settings.proxy_core_path)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn persist_proxy_core_path(db: &crate::storage::db::Db, path: Option<&str>) -> AppResult<()> {
    let Some(path) = path.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let settings = settings_repo::get(db)?;
    if settings
        .proxy_core_path
        .as_deref()
        .map(str::trim)
        == Some(path)
    {
        return Ok(());
    }
    settings_repo::save(
        db,
        crate::domain::settings::SaveSettingsInput {
            chrome_path: settings.chrome_path,
            camoufox_python_path: settings.camoufox_python_path,
            proxy_core_path: Some(path.to_string()),
            default_concurrency: settings.default_concurrency,
            default_locale: settings.default_locale,
            default_timezone_id: settings.default_timezone_id,
            default_viewport_width: settings.default_viewport_width,
            default_viewport_height: settings.default_viewport_height,
            aigc_base_url: settings.aigc_base_url,
            aigc_model: settings.aigc_model,
            aigc_api_key: settings.aigc_api_key,
        },
    )?;
    Ok(())
}


#[tauri::command]
pub fn list_proxy_subscriptions(state: State<'_, AppState>) -> AppResult<Vec<ProxySubscription>> {
    subscription_repo::list_subscriptions(state.db())
}

#[tauri::command]
pub fn save_proxy_subscription(
    state: State<'_, AppState>,
    input: SaveProxySubscriptionInput,
) -> AppResult<ProxySubscription> {
    subscription_repo::save_subscription(state.db(), input)
}

#[tauri::command]
pub fn delete_proxy_subscription(state: State<'_, AppState>, id: String) -> AppResult<()> {
    let nodes = subscription_repo::list_nodes(state.db(), Some(&id))?;
    for node in nodes {
        if let Err(err) = core_manager::stop_local_proxy_for_data_dir(state.data_dir(), &node.id) {
            tracing::warn!(
                node_id = %node.id,
                error = %err,
                "Failed to stop subscription node proxy while deleting subscription"
            );
        }
    }
    subscription_repo::delete_subscription(state.db(), &id)
}

#[tauri::command]
pub fn list_proxy_subscription_nodes(
    state: State<'_, AppState>,
    subscription_id: Option<String>,
) -> AppResult<Vec<ProxySubscriptionNode>> {
    subscription_repo::list_nodes(state.db(), subscription_id.as_deref())
}

#[tauri::command]
pub async fn sync_proxy_subscription(
    state: State<'_, AppState>,
    id: String,
) -> AppResult<ProxySubscriptionSyncResult> {
    let subscription = subscription_repo::get_subscription(state.db(), &id)?.ok_or_else(|| {
        AppError::new("subscription_not_found", "Subscription does not exist")
    })?;

    match fetch_subscription_nodes(&subscription.url).await {
        Ok(parsed) => {
            let imported = parsed.len() as i64;
            let skipped = parsed.iter().filter(|node| node.is_meta).count() as i64;
            let nodes = subscription_repo::replace_nodes(state.db(), &id, &parsed)?;
            let subscription = subscription_repo::get_subscription(state.db(), &id)?.ok_or_else(|| {
                AppError::new(
                    "subscription_not_found",
                    "Subscription disappeared after sync",
                )
            })?;
            Ok(ProxySubscriptionSyncResult {
                subscription,
                nodes,
                imported,
                skipped,
            })
        }
        Err(err) => {
            let _ = subscription_repo::mark_sync_error(state.db(), &id, &err.message);
            Err(err)
        }
    }
}

#[tauri::command]
pub async fn sync_due_proxy_subscriptions(
    state: State<'_, AppState>,
) -> AppResult<Vec<ProxySubscriptionSyncResult>> {
    let subscriptions = subscription_repo::list_subscriptions(state.db())?;
    let now = chrono::Utc::now();
    let mut results = Vec::new();

    for subscription in subscriptions {
        if !subscription.enabled || subscription.auto_sync_interval_minutes <= 0 {
            continue;
        }
        let due = match subscription.last_synced_at.as_deref() {
            None => true,
            Some(value) => match chrono::DateTime::parse_from_rfc3339(value) {
                Ok(parsed) => {
                    let elapsed = now.signed_duration_since(parsed.with_timezone(&chrono::Utc));
                    elapsed.num_minutes() >= subscription.auto_sync_interval_minutes
                }
                Err(_) => true,
            },
        };
        if !due {
            continue;
        }
        match sync_proxy_subscription(state.clone(), subscription.id.clone()).await {
            Ok(result) => results.push(result),
            Err(err) => {
                tracing::warn!(
                    subscription_id = %subscription.id,
                    error = %err,
                    "auto subscription sync failed"
                );
            }
        }
    }

    Ok(results)
}

#[tauri::command]
pub fn detect_proxy_core(state: State<'_, AppState>) -> AppResult<ProxyCoreStatus> {
    let override_path = proxy_core_override(state.db());
    let status = core_manager::detect_core(override_path.as_deref());
    if status.found {
        let _ = persist_proxy_core_path(state.db(), status.path.as_deref());
    }
    Ok(status)
}

#[tauri::command]
pub async fn ensure_proxy_core(state: State<'_, AppState>) -> AppResult<ProxyCoreStatus> {
    let override_path = proxy_core_override(state.db());
    let status = core_manager::ensure_core_installed(override_path.as_deref()).await?;
    if status.found {
        persist_proxy_core_path(state.db(), status.path.as_deref())?;
    }
    Ok(status)
}

#[tauri::command]
pub async fn apply_subscription_node_to_environment(
    state: State<'_, AppState>,
    input: ApplySubscriptionNodeInput,
) -> AppResult<crate::domain::environment::Environment> {
    let environment = environment_repo::get(state.db(), &input.environment_id)?;
    let node = subscription_repo::get_node(state.db(), &input.node_id)?.ok_or_else(|| {
        AppError::new(
            "subscription_node_not_found",
            "Subscription node does not exist",
        )
    })?;

    let proxy_config =
        materialize_node_proxy_config(state.data_dir(), &node, &environment.proxy_config, state.db())?;
    let saved = environment_repo::save(
        state.db(),
        SaveEnvironmentInput {
            id: Some(environment.id.clone()),
            name: environment.name,
            group_id: environment.group_id,
            tags: environment.tags,
            notes: environment.notes,
            browser_kind: environment.browser_kind,
            chrome_path_override: environment.chrome_path_override,
            proxy_config,
            locale: environment.locale,
            timezone_id: environment.timezone_id,
            geolocation_latitude: environment.geolocation_latitude,
            geolocation_longitude: environment.geolocation_longitude,
            user_agent: environment.user_agent,
            platform: environment.platform,
            web_rtc_protection: environment.web_rtc_protection,
            viewport_width: environment.viewport_width,
            viewport_height: environment.viewport_height,
            device_scale_factor: environment.device_scale_factor,
            environment_mode: environment.environment_mode,
            seed: environment.seed,
            headless: environment.headless,
            start_url: environment.start_url,
        },
    )?;
    Ok(saved)
}

#[tauri::command]
pub async fn create_environments_from_subscription_nodes(
    state: State<'_, AppState>,
    input: CreateEnvironmentsFromNodesInput,
) -> AppResult<CreateEnvironmentsFromNodesResult> {
    let subscription = subscription_repo::get_subscription(state.db(), &input.subscription_id)?
        .ok_or_else(|| AppError::new("subscription_not_found", "Subscription does not exist"))?;
    let settings = settings_repo::get(state.db())?;
    let mut nodes = subscription_repo::list_nodes(state.db(), Some(&input.subscription_id))?;
    if let Some(node_ids) = input.node_ids.as_ref() {
        let wanted = node_ids.iter().cloned().collect::<std::collections::HashSet<_>>();
        nodes.retain(|node| wanted.contains(&node.id));
    }
    if input.only_usable {
        nodes.retain(|node| !node.is_meta);
    }
    if nodes.is_empty() {
        return Err(AppError::new(
            "subscription_no_nodes",
            "No usable nodes selected for environment creation",
        ));
    }

    let prefix = input
        .name_prefix
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(subscription.name.as_str());
    let group_id = input
        .group_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| Some(subscription.name.clone()));

    let mut created = Vec::new();
    let skipped = 0_i64;
    for node in nodes {
        // Create durable node binding first; local core is started later on launch/test.
        let proxy_config = if matches!(
            node.protocol,
            SubscriptionNodeProtocol::Http
                | SubscriptionNodeProtocol::Https
                | SubscriptionNodeProtocol::Socks4
                | SubscriptionNodeProtocol::Socks5
        ) {
            materialize_node_proxy_config(
                state.data_dir(),
                &node,
                &ProxyConfig {
                    kind: ProxyKind::None,
                    host: None,
                    port: None,
                    username: None,
                    password: None,
                    bypass_list: vec![
                        "localhost".to_string(),
                        "127.0.0.1".to_string(),
                        "::1".to_string(),
                    ],
                },
                state.db(),
            )?
        } else {
            ProxyConfig {
                kind: ProxyKind::Socks5,
                host: Some("127.0.0.1".to_string()),
                port: None,
                username: Some(format!("orbit-node:{}", node.id)),
                password: Some(node.name.clone()),
                bypass_list: vec![
                    "localhost".to_string(),
                    "127.0.0.1".to_string(),
                    "::1".to_string(),
                ],
            }
        };

        let mut tags = node.tags.clone();
        tags.push("subscription".to_string());
        tags.push(node.protocol_label());
        tags.sort();
        tags.dedup();

        let environment = environment_repo::save(
            state.db(),
            SaveEnvironmentInput {
                id: None,
                name: format!("{prefix} · {}", node.name),
                group_id: group_id.clone(),
                tags,
                notes: Some(format!(
                    "Created from subscription `{}` node `{}` ({})",
                    subscription.name, node.name, node.share_uri
                )),
                browser_kind: BrowserKind::Chrome,
                chrome_path_override: None,
                proxy_config,
                locale: settings.default_locale.clone(),
                timezone_id: Some(settings.default_timezone_id.clone()),
                geolocation_latitude: None,
                geolocation_longitude: None,
                user_agent: None,
                platform: Some("auto".to_string()),
                web_rtc_protection: true,
                viewport_width: settings.default_viewport_width,
                viewport_height: settings.default_viewport_height,
                device_scale_factor: 1.0,
                environment_mode: EnvironmentMode::Standard,
                seed: None,
                headless: false,
                start_url: Some("about:blank".to_string()),
            },
        )?;
        created.push(environment);
    }

    Ok(CreateEnvironmentsFromNodesResult { created, skipped })
}

#[tauri::command]
pub async fn probe_subscription_nodes(
    state: State<'_, AppState>,
    input: ProbeSubscriptionNodesInput,
) -> AppResult<ProbeSubscriptionNodesResult> {
    let mut nodes = subscription_repo::list_nodes(state.db(), Some(&input.subscription_id))?;
    if let Some(node_ids) = input.node_ids.as_ref() {
        let wanted = node_ids.iter().cloned().collect::<std::collections::HashSet<_>>();
        nodes.retain(|node| wanted.contains(&node.id));
    }
    if input.only_usable {
        nodes.retain(|node| !node.is_meta);
    }

    let mut join_set = JoinSet::new();
    for node in nodes {
        join_set.spawn(async move { probe_node_latency(node).await });
    }

    let mut results = Vec::new();
    while let Some(joined) = join_set.join_next().await {
        match joined {
            Ok(item) => {
                let _ = subscription_repo::update_node_latency(
                    state.db(),
                    &item.node_id,
                    item.latency_ms,
                );
                results.push(item);
            }
            Err(err) => {
                tracing::warn!(error = %err, "latency probe task failed");
            }
        }
    }
    results.sort_by(|left, right| match (left.latency_ms, right.latency_ms) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => left.name.cmp(&right.name),
    });
    Ok(ProbeSubscriptionNodesResult { results })
}

#[tauri::command]
pub async fn ensure_subscription_node_proxy(
    state: State<'_, AppState>,
    node_id: String,
) -> AppResult<LocalProxyEndpoint> {
    let node = subscription_repo::get_node(state.db(), &node_id)?.ok_or_else(|| {
        AppError::new(
            "subscription_node_not_found",
            "Subscription node does not exist",
        )
    })?;
    core_manager::ensure_local_proxy_for_node(
        state.data_dir(),
        &node,
        proxy_core_override(state.db()).as_deref(),
    )
}

pub fn materialize_node_proxy_config(
    data_dir: &std::path::Path,
    node: &ProxySubscriptionNode,
    current: &ProxyConfig,
    db: &crate::storage::db::Db,
) -> AppResult<ProxyConfig> {
    let bypass_list = if current.bypass_list.is_empty() {
        vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "::1".to_string(),
        ]
    } else {
        current.bypass_list.clone()
    };

    if matches!(
        node.protocol,
        SubscriptionNodeProtocol::Http
            | SubscriptionNodeProtocol::Https
            | SubscriptionNodeProtocol::Socks4
            | SubscriptionNodeProtocol::Socks5
    ) {
        let config: Value = serde_json::from_str(&node.config_json).unwrap_or(json!({}));
        let kind = match node.protocol {
            SubscriptionNodeProtocol::Https => ProxyKind::Https,
            SubscriptionNodeProtocol::Socks4 => ProxyKind::Socks4,
            SubscriptionNodeProtocol::Socks5 => ProxyKind::Socks5,
            _ => ProxyKind::Http,
        };
        return Ok(ProxyConfig {
            kind,
            host: Some(
                config
                    .get("host")
                    .and_then(Value::as_str)
                    .unwrap_or(node.server.as_str())
                    .to_string(),
            ),
            port: Some(
                config
                    .get("port")
                    .and_then(Value::as_u64)
                    .map(|value| value as u16)
                    .unwrap_or(node.port),
            ),
            username: config
                .get("username")
                .and_then(Value::as_str)
                .map(str::to_string),
            password: config
                .get("password")
                .and_then(Value::as_str)
                .map(str::to_string),
            bypass_list,
        });
    }

    // 协议节点：必须拉起本地内核并返回可用的 socks5 入口。
    let endpoint = core_manager::ensure_local_proxy_for_node(
        data_dir,
        node,
        proxy_core_override(db).as_deref(),
    )?;
    Ok(ProxyConfig {
        kind: endpoint.kind,
        host: Some(endpoint.host),
        port: Some(endpoint.port),
        // 保留绑定信息，下次启动可重新 hydrate。
        username: Some(format!("orbit-node:{}", node.id)),
        password: Some(node.name.clone()),
        bypass_list,
    })
}

pub fn ensure_environment_proxy_ready(
    data_dir: &std::path::Path,
    proxy_config: &ProxyConfig,
    db: &crate::storage::db::Db,
) -> AppResult<ProxyConfig> {
    let Some(node_id) = extract_bound_node_id(proxy_config) else {
        return Ok(proxy_config.clone());
    };
    let node = subscription_repo::get_node(db, &node_id)?.ok_or_else(|| {
        AppError::new(
            "subscription_node_not_found",
            "Bound subscription node no longer exists. Re-select a node for this environment.",
        )
    })?;
    materialize_node_proxy_config(data_dir, &node, proxy_config, db)
}

pub fn extract_bound_node_id(proxy_config: &ProxyConfig) -> Option<String> {
    proxy_config.bound_subscription_node_id()
}


async fn fetch_subscription_nodes(
    url: &str,
) -> AppResult<Vec<crate::proxy::subscription_parser::ParsedSubscriptionNode>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?;
    let response = client
        .get(url)
        .header(
            "User-Agent",
            "OrbitBrowser/0.5 subscription-sync (+https://github.com/)",
        )
        .send()
        .await
        .map_err(|err| {
            AppError::new(
                "subscription_fetch_failed",
                format!("Failed to fetch subscription: {err}"),
            )
            .retryable(true)
        })?;
    if !response.status().is_success() {
        return Err(AppError::new(
            "subscription_fetch_failed",
            format!("Subscription endpoint returned HTTP {}", response.status()),
        )
        .retryable(true));
    }
    let bytes = response.bytes().await.map_err(|err| {
        AppError::new(
            "subscription_fetch_failed",
            format!("Failed to read subscription body: {err}"),
        )
        .retryable(true)
    })?;
    let content = subscription_parser::decode_subscription_body(&bytes)?;
    subscription_parser::parse_subscription_content(&content)
}

async fn probe_node_latency(node: ProxySubscriptionNode) -> SubscriptionNodeLatency {
    let target = format!("{}:{}", node.server, node.port);
    let started = Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        let addrs = target
            .to_socket_addrs()
            .map_err(|err| format!("DNS failed: {err}"))?;
        let mut last_error = "connection failed".to_string();
        for addr in addrs {
            match std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
                Ok(stream) => {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    return Ok(());
                }
                Err(err) => last_error = err.to_string(),
            }
        }
        Err(last_error)
    })
    .await;

    match result {
        Ok(Ok(())) => SubscriptionNodeLatency {
            node_id: node.id,
            name: node.name,
            ok: true,
            latency_ms: Some(started.elapsed().as_millis() as i64),
            message: "reachable".to_string(),
        },
        Ok(Err(message)) => SubscriptionNodeLatency {
            node_id: node.id,
            name: node.name,
            ok: false,
            latency_ms: None,
            message,
        },
        Err(err) => SubscriptionNodeLatency {
            node_id: node.id,
            name: node.name,
            ok: false,
            latency_ms: None,
            message: format!("probe failed: {err}"),
        },
    }
}

trait ProtocolLabel {
    fn protocol_label(&self) -> String;
}

impl ProtocolLabel for ProxySubscriptionNode {
    fn protocol_label(&self) -> String {
        match self.protocol {
            SubscriptionNodeProtocol::Http => "http",
            SubscriptionNodeProtocol::Https => "https",
            SubscriptionNodeProtocol::Socks4 => "socks4",
            SubscriptionNodeProtocol::Socks5 => "socks5",
            SubscriptionNodeProtocol::Shadowsocks => "shadowsocks",
            SubscriptionNodeProtocol::Trojan => "trojan",
            SubscriptionNodeProtocol::Vmess => "vmess",
            SubscriptionNodeProtocol::Vless => "vless",
            SubscriptionNodeProtocol::Hysteria2 => "hysteria2",
            SubscriptionNodeProtocol::Unknown => "unknown",
        }
        .to_string()
    }
}
