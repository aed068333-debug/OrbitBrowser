use crate::domain::proxy::ProxyKind;
use crate::domain::subscription::{
    LocalProxyEndpoint, ProxyCoreStatus, ProxySubscriptionNode, SubscriptionNodeProtocol,
};
use crate::errors::{AppError, AppResult};
use crate::proxy::{core_installer, port_allocator};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoreKind {
    Xray,
    SingBox,
}

impl CoreKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Xray => "xray",
            Self::SingBox => "sing-box",
        }
    }
}

#[derive(Debug)]
struct ManagedLocalProxy {
    endpoint: LocalProxyEndpoint,
    child: Child,
}

fn registry() -> &'static Mutex<HashMap<String, ManagedLocalProxy>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, ManagedLocalProxy>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn ensure_core_installed(override_path: Option<&str>) -> AppResult<ProxyCoreStatus> {
    if let Some(path) = override_path.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(detect_core(Some(path)));
    }
    let status = detect_core(None);
    if status.found {
        return Ok(status);
    }
    let path = core_installer::ensure_managed_xray().await?;
    Ok(detect_core(Some(path.to_string_lossy().as_ref())))
}

pub fn detect_core(override_path: Option<&str>) -> ProxyCoreStatus {
    if let Some(path) = override_path.map(str::trim).filter(|value| !value.is_empty()) {
        match inspect_core_path(Path::new(path)) {
            Ok((kind, version)) => {
                return ProxyCoreStatus {
                    found: true,
                    path: Some(path.to_string()),
                    kind: Some(kind.as_str().to_string()),
                    version: Some(version),
                    searched_paths: vec![path.to_string()],
                    message: None,
                };
            }
            Err(err) => {
                return ProxyCoreStatus {
                    found: false,
                    path: Some(path.to_string()),
                    kind: None,
                    version: None,
                    searched_paths: vec![path.to_string()],
                    message: Some(err.message),
                };
            }
        }
    }

    let candidates = default_core_candidates();
    let searched_paths = candidates
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    for candidate in &candidates {
        if let Ok((kind, version)) = inspect_core_path(candidate) {
            return ProxyCoreStatus {
                found: true,
                path: Some(candidate.to_string_lossy().to_string()),
                kind: Some(kind.as_str().to_string()),
                version: Some(version),
                searched_paths,
                message: None,
            };
        }
    }

    ProxyCoreStatus {
        found: false,
        path: None,
        kind: None,
        version: None,
        searched_paths,
        message: Some(
            "No local proxy core found. Orbit will download managed Xray automatically, or set ORBIT_PROXY_CORE."
                .to_string(),
        ),
    }
}


fn futures_executor_block_on_install() -> AppResult<PathBuf> {
    // Avoid Handle::block_on inside an existing runtime; spawn a dedicated runtime thread.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = tauri::async_runtime::block_on(core_installer::ensure_managed_xray());
        let _ = tx.send(result);
    });
    rx.recv().map_err(|err| {
        AppError::new(
            "proxy_core_install_failed",
            format!("Failed to install managed xray core: {err}"),
        )
        .retryable(true)
    })?
}

pub fn ensure_local_proxy_for_node(
    data_dir: &Path,
    node: &ProxySubscriptionNode,
    core_override: Option<&str>,
) -> AppResult<LocalProxyEndpoint> {
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
        return Ok(LocalProxyEndpoint {
            host: config
                .get("host")
                .and_then(Value::as_str)
                .unwrap_or(node.server.as_str())
                .to_string(),
            port: config
                .get("port")
                .and_then(Value::as_u64)
                .map(|value| value as u16)
                .unwrap_or(node.port),
            kind,
            node_id: node.id.clone(),
            node_name: node.name.clone(),
            core_kind: "direct".to_string(),
            config_path: String::new(),
        });
    }

    {
        let mut guard = registry()
            .lock()
            .map_err(|_| AppError::new("proxy_core_lock_failed", "Failed to lock proxy core registry"))?;
        if let Some(existing) = guard.get_mut(&node.id) {
            if process_alive(&mut existing.child) {
                return Ok(existing.endpoint.clone());
            }
            let _ = existing.child.kill();
            let _ = existing.child.wait();
            guard.remove(&node.id);
        }
    }

    let mut detection = detect_core(core_override);
    if detection.path.is_none() && core_override.map(str::trim).filter(|v| !v.is_empty()).is_none() {
        // 自动下载并内置 xray 到应用数据目录。
        match futures_executor_block_on_install() {
            Ok(path) => {
                detection = detect_core(Some(path.to_string_lossy().as_ref()));
            }
            Err(err) => {
                return Err(err);
            }
        }
    }
    let core_path = detection.path.clone().ok_or_else(|| {
        AppError::new(
            "proxy_core_not_found",
            detection
                .message
                .unwrap_or_else(|| "Local proxy core was not found".to_string()),
        )
        .details(json!({ "searchedPaths": detection.searched_paths }))
        .retryable(true)
    })?;
    let core_kind = match detection.kind.as_deref() {
        Some("sing-box") => CoreKind::SingBox,
        _ => CoreKind::Xray,
    };

    let local_port = port_allocator::allocate()?;
    let config_dir = data_dir.join("runtime").join("proxy-cores").join(&node.id);
    std::fs::create_dir_all(&config_dir)?;
    let config_path = match core_kind {
        CoreKind::Xray => {
            let path = config_dir.join("config.json");
            std::fs::write(&path, serde_json::to_vec_pretty(&build_xray_config(node, local_port)?)?)?;
            path
        }
        CoreKind::SingBox => {
            let path = config_dir.join("config.json");
            std::fs::write(
                &path,
                serde_json::to_vec_pretty(&build_singbox_config(node, local_port)?)?,
            )?;
            path
        }
    };

    let mut command = Command::new(&core_path);
    match core_kind {
        CoreKind::Xray => {
            command.arg("run").arg("-c").arg(&config_path);
        }
        CoreKind::SingBox => {
            command.arg("run").arg("-c").arg(&config_path);
        }
    }
    let stderr_path = config_dir.join("core.stderr.log");
    let stderr_file = std::fs::File::create(&stderr_path).map_err(|err| {
        AppError::new(
            "proxy_core_start_failed",
            format!("Failed to create proxy core log file: {err}"),
        )
        .retryable(true)
    })?;
    command
        .current_dir(&config_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file));
    configure_core_process(&mut command);

    let mut child = command.spawn().map_err(|err| {
        AppError::new(
            "proxy_core_start_failed",
            format!("Failed to start local proxy core: {err}"),
        )
        .details(json!({ "path": core_path, "config": config_path }))
        .retryable(true)
    })?;

    if let Err(err) = wait_for_port(local_port, Duration::from_secs(8)) {
        let _ = child.kill();
        let _ = child.wait();
        let stderr_text = std::fs::read_to_string(&stderr_path).unwrap_or_default();
        let detail = stderr_text
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or(err.message.as_str())
            .to_string();
        return Err(AppError::new(
            "proxy_core_start_failed",
            format!("Local proxy core failed to start: {detail}"),
        )
        .details(json!({
            "path": core_path,
            "config": config_path,
            "stderr": stderr_text.chars().take(2000).collect::<String>(),
            "port": local_port,
        }))
        .retryable(true));
    }

    let endpoint = LocalProxyEndpoint {
        host: "127.0.0.1".to_string(),
        port: local_port,
        kind: ProxyKind::Socks5,
        node_id: node.id.clone(),
        node_name: node.name.clone(),
        core_kind: core_kind.as_str().to_string(),
        config_path: config_path.to_string_lossy().to_string(),
    };

    let mut guard = registry()
        .lock()
        .map_err(|_| AppError::new("proxy_core_lock_failed", "Failed to lock proxy core registry"))?;
    guard.insert(
        node.id.clone(),
        ManagedLocalProxy {
            endpoint: endpoint.clone(),
            child,
        },
    );
    Ok(endpoint)
}

#[allow(dead_code)]
pub fn stop_local_proxy(node_id: &str) -> AppResult<()> {
    let mut guard = registry()
        .lock()
        .map_err(|_| AppError::new("proxy_core_lock_failed", "Failed to lock proxy core registry"))?;
    if let Some(mut managed) = guard.remove(node_id) {
        let _ = managed.child.kill();
        let _ = managed.child.wait();
    }
    Ok(())
}

fn build_xray_config(node: &ProxySubscriptionNode, local_port: u16) -> AppResult<Value> {
    let config: Value = serde_json::from_str(&node.config_json)?;
    let outbound = match &node.protocol {
        SubscriptionNodeProtocol::Vless => build_xray_vless_outbound(&config)?,
        SubscriptionNodeProtocol::Vmess => build_xray_vmess_outbound(&config)?,
        SubscriptionNodeProtocol::Trojan => build_xray_trojan_outbound(&config)?,
        SubscriptionNodeProtocol::Shadowsocks => build_xray_shadowsocks_outbound(&config)?,
        other => {
            return Err(AppError::new(
                "subscription_unsupported",
                format!("Protocol {other:?} requires a local core path that is not configured for xray"),
            ));
        }
    };

    Ok(json!({
        "log": { "loglevel": "warning" },
        "inbounds": [{
            "tag": "socks-in",
            "listen": "127.0.0.1",
            "port": local_port,
            "protocol": "socks",
            "settings": {
                "udp": true,
                "auth": "noauth"
            },
            "sniffing": {
                "enabled": true,
                "destOverride": ["http", "tls"]
            }
        }],
        "outbounds": [
            outbound,
            {
                "tag": "direct",
                "protocol": "freedom",
                "settings": {}
            },
            {
                "tag": "block",
                "protocol": "blackhole",
                "settings": {}
            }
        ]
    }))
}

fn build_singbox_config(node: &ProxySubscriptionNode, local_port: u16) -> AppResult<Value> {
    let config: Value = serde_json::from_str(&node.config_json)?;
    let outbound = match &node.protocol {
        SubscriptionNodeProtocol::Vless => build_singbox_vless_outbound(&config)?,
        SubscriptionNodeProtocol::Vmess => build_singbox_vmess_outbound(&config)?,
        SubscriptionNodeProtocol::Trojan => build_singbox_trojan_outbound(&config)?,
        SubscriptionNodeProtocol::Shadowsocks => build_singbox_shadowsocks_outbound(&config)?,
        SubscriptionNodeProtocol::Hysteria2 => build_singbox_hysteria2_outbound(&config)?,
        other => {
            return Err(AppError::new(
                "subscription_unsupported",
                format!("Protocol {other:?} is not supported by sing-box bridge yet"),
            ));
        }
    };

    Ok(json!({
        "log": { "level": "warn" },
        "inbounds": [{
            "type": "socks",
            "tag": "socks-in",
            "listen": "127.0.0.1",
            "listen_port": local_port,
            "udp": true
        }],
        "outbounds": [
            outbound,
            { "type": "direct", "tag": "direct" },
            { "type": "block", "tag": "block" }
        ]
    }))
}

fn build_xray_vless_outbound(config: &Value) -> AppResult<Value> {
    let server = required_str(config, "server")?;
    let port = required_u16(config, "port")?;
    let uuid = required_str(config, "uuid")?;
    let network = config
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("tcp");
    let security = config
        .get("security")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let flow = config.get("flow").and_then(Value::as_str).unwrap_or("");

    let mut user = json!({
        "id": uuid,
        "encryption": config.get("encryption").and_then(Value::as_str).unwrap_or("none"),
    });
    if !flow.is_empty() {
        user["flow"] = json!(flow);
    }

    let mut stream = json!({
        "network": network,
        "security": security,
    });
    if network == "ws" {
        stream["wsSettings"] = json!({
            "path": config.get("path").and_then(Value::as_str).unwrap_or("/"),
            "headers": {
                "Host": config.get("host").and_then(Value::as_str).unwrap_or("")
            }
        });
    } else if network == "grpc" {
        stream["grpcSettings"] = json!({
            "serviceName": config.get("serviceName").and_then(Value::as_str).unwrap_or("")
        });
    }

    if security == "tls" {
        let mut tls = json!({
            "serverName": config.get("sni").and_then(Value::as_str).unwrap_or(server),
            "fingerprint": config.get("fp").and_then(Value::as_str).unwrap_or("chrome"),
            // 订阅节点多为自签证书；配合 Xray 25.x 的 allowInsecure。
            "allowInsecure": true,
        });
        if let Some(alpn) = config.get("alpn").and_then(Value::as_str) {
            tls["alpn"] = json!(alpn.split(',').map(str::trim).filter(|item| !item.is_empty()).collect::<Vec<_>>());
        }
        stream["tlsSettings"] = tls;
    } else if security == "reality" {
        let mut reality = json!({
            "serverName": config.get("sni").and_then(Value::as_str).unwrap_or(server),
            "fingerprint": config.get("fp").and_then(Value::as_str).unwrap_or("chrome"),
            "publicKey": config.get("pbk").and_then(Value::as_str).unwrap_or(""),
            "shortId": config.get("sid").and_then(Value::as_str).unwrap_or(""),
            "spiderX": config.get("spx").and_then(Value::as_str).unwrap_or(""),
        });
        if let Some(alpn) = config.get("alpn").and_then(Value::as_str) {
            reality["alpn"] = json!(alpn.split(',').map(str::trim).filter(|item| !item.is_empty()).collect::<Vec<_>>());
        }
        stream["security"] = json!("reality");
        stream["realitySettings"] = reality;
    }

    Ok(json!({
        "tag": "proxy",
        "protocol": "vless",
        "settings": {
            "vnext": [{
                "address": server,
                "port": port,
                "users": [user]
            }]
        },
        "streamSettings": stream
    }))
}

fn build_xray_vmess_outbound(config: &Value) -> AppResult<Value> {
    let server = required_str(config, "server")
        .or_else(|_| required_str(config, "add"))?;
    let port = required_u16(config, "port")?;
    let uuid = required_str(config, "id")?;
    let network = config
        .get("net")
        .or_else(|| config.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("tcp");
    let security = config
        .get("tls")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("none");
    let mut stream = json!({
        "network": network,
        "security": if security == "tls" { "tls" } else { "none" },
    });
    if network == "ws" {
        stream["wsSettings"] = json!({
            "path": config.get("path").and_then(Value::as_str).unwrap_or("/"),
            "headers": {
                "Host": config.get("host").and_then(Value::as_str).unwrap_or("")
            }
        });
    }
    if security == "tls" {
        stream["tlsSettings"] = json!({
            "serverName": config.get("sni").and_then(Value::as_str).unwrap_or(server),
            "fingerprint": config.get("fp").and_then(Value::as_str).unwrap_or("chrome"),
            "allowInsecure": true,
        });
    }
    Ok(json!({
        "tag": "proxy",
        "protocol": "vmess",
        "settings": {
            "vnext": [{
                "address": server,
                "port": port,
                "users": [{
                    "id": uuid,
                    "alterId": config.get("aid").and_then(|value| value.as_u64()).unwrap_or(0),
                    "security": config.get("scy").and_then(Value::as_str).unwrap_or("auto")
                }]
            }]
        },
        "streamSettings": stream
    }))
}

fn build_xray_trojan_outbound(config: &Value) -> AppResult<Value> {
    let server = required_str(config, "server")?;
    let port = required_u16(config, "port")?;
    let password = required_str(config, "password")?;
    Ok(json!({
        "tag": "proxy",
        "protocol": "trojan",
        "settings": {
            "servers": [{
                "address": server,
                "port": port,
                "password": password
            }]
        },
        "streamSettings": {
            "network": config.get("type").and_then(Value::as_str).unwrap_or("tcp"),
            "security": "tls",
            "tlsSettings": {
                "serverName": config.get("sni").and_then(Value::as_str).unwrap_or(server),
                "fingerprint": config.get("fp").and_then(Value::as_str).unwrap_or("chrome"),
                "allowInsecure": true
            }
        }
    }))
}

fn build_xray_shadowsocks_outbound(config: &Value) -> AppResult<Value> {
    Ok(json!({
        "tag": "proxy",
        "protocol": "shadowsocks",
        "settings": {
            "servers": [{
                "address": required_str(config, "server")?,
                "port": required_u16(config, "port")?,
                "method": required_str(config, "method")?,
                "password": required_str(config, "password")?
            }]
        }
    }))
}

fn build_singbox_vless_outbound(config: &Value) -> AppResult<Value> {
    let mut outbound = json!({
        "type": "vless",
        "tag": "proxy",
        "server": required_str(config, "server")?,
        "server_port": required_u16(config, "port")?,
        "uuid": required_str(config, "uuid")?,
        "packet_encoding": "xudp"
    });
    if let Some(flow) = config.get("flow").and_then(Value::as_str).filter(|value| !value.is_empty()) {
        outbound["flow"] = json!(flow);
    }
    attach_singbox_transport(&mut outbound, config);
    attach_singbox_tls(&mut outbound, config)?;
    Ok(outbound)
}

fn build_singbox_vmess_outbound(config: &Value) -> AppResult<Value> {
    let mut outbound = json!({
        "type": "vmess",
        "tag": "proxy",
        "server": required_str(config, "server").or_else(|_| required_str(config, "add"))?,
        "server_port": required_u16(config, "port")?,
        "uuid": required_str(config, "id")?,
        "security": config.get("scy").and_then(Value::as_str).unwrap_or("auto"),
        "alter_id": config.get("aid").and_then(Value::as_u64).unwrap_or(0)
    });
    attach_singbox_transport(&mut outbound, config);
    if config.get("tls").and_then(Value::as_str) == Some("tls") {
        attach_singbox_tls(&mut outbound, config)?;
    }
    Ok(outbound)
}

fn build_singbox_trojan_outbound(config: &Value) -> AppResult<Value> {
    let mut outbound = json!({
        "type": "trojan",
        "tag": "proxy",
        "server": required_str(config, "server")?,
        "server_port": required_u16(config, "port")?,
        "password": required_str(config, "password")?
    });
    attach_singbox_transport(&mut outbound, config);
    attach_singbox_tls(&mut outbound, config)?;
    Ok(outbound)
}

fn build_singbox_shadowsocks_outbound(config: &Value) -> AppResult<Value> {
    Ok(json!({
        "type": "shadowsocks",
        "tag": "proxy",
        "server": required_str(config, "server")?,
        "server_port": required_u16(config, "port")?,
        "method": required_str(config, "method")?,
        "password": required_str(config, "password")?
    }))
}

fn build_singbox_hysteria2_outbound(config: &Value) -> AppResult<Value> {
    let mut outbound = json!({
        "type": "hysteria2",
        "tag": "proxy",
        "server": required_str(config, "server")?,
        "server_port": required_u16(config, "port")?,
        "password": required_str(config, "password")?
    });
    attach_singbox_tls(&mut outbound, config)?;
    Ok(outbound)
}

fn attach_singbox_transport(outbound: &mut Value, config: &Value) {
    let network = config
        .get("type")
        .or_else(|| config.get("net"))
        .and_then(Value::as_str)
        .unwrap_or("tcp");
    if network == "ws" {
        outbound["transport"] = json!({
            "type": "ws",
            "path": config.get("path").and_then(Value::as_str).unwrap_or("/"),
            "headers": {
                "Host": config.get("host").and_then(Value::as_str).unwrap_or("")
            }
        });
    } else if network == "grpc" {
        outbound["transport"] = json!({
            "type": "grpc",
            "service_name": config.get("serviceName").and_then(Value::as_str).unwrap_or("")
        });
    }
}

fn attach_singbox_tls(outbound: &mut Value, config: &Value) -> AppResult<()> {
    let security = config
        .get("security")
        .and_then(Value::as_str)
        .unwrap_or("tls");
    if security == "none" {
        return Ok(());
    }
    let server = required_str(config, "server").unwrap_or("");
    let mut tls = json!({
        "enabled": true,
        "server_name": config.get("sni").and_then(Value::as_str).unwrap_or(server),
        "insecure": truthy(config.get("insecure")) || truthy(config.get("allowInsecure")),
        "utls": {
            "enabled": true,
            "fingerprint": config.get("fp").and_then(Value::as_str).unwrap_or("chrome")
        }
    });
    if security == "reality" {
        tls["reality"] = json!({
            "enabled": true,
            "public_key": config.get("pbk").and_then(Value::as_str).unwrap_or(""),
            "short_id": config.get("sid").and_then(Value::as_str).unwrap_or("")
        });
    }
    outbound["tls"] = tls;
    Ok(())
}

fn required_str<'a>(config: &'a Value, key: &str) -> AppResult<&'a str> {
    config
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::new("subscription_invalid", format!("Missing field: {key}")))
}

fn required_u16(config: &Value, key: &str) -> AppResult<u16> {
    config
        .get(key)
        .and_then(|value| match value {
            Value::Number(number) => number.as_u64().map(|item| item as u16),
            Value::String(text) => text.parse().ok(),
            _ => None,
        })
        .ok_or_else(|| AppError::new("subscription_invalid", format!("Missing field: {key}")))
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_u64().unwrap_or(0) != 0,
        Some(Value::String(text)) => {
            let lower = text.trim().to_ascii_lowercase();
            matches!(lower.as_str(), "1" | "true" | "yes" | "on")
        }
        _ => false,
    }
}

fn inspect_core_path(path: &Path) -> AppResult<(CoreKind, String)> {
    if !path.exists() {
        return Err(AppError::new("proxy_core_not_found", "Proxy core path does not exist"));
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let kind = if file_name.contains("sing-box") || file_name.contains("singbox") {
        CoreKind::SingBox
    } else if file_name.contains("xray") {
        CoreKind::Xray
    } else {
        // Prefer probing version output.
        if let Ok(output) = Command::new(path).arg("version").output() {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .to_ascii_lowercase();
            if text.contains("sing-box") {
                CoreKind::SingBox
            } else {
                CoreKind::Xray
            }
        } else {
            CoreKind::Xray
        }
    };

    let version = match kind {
        CoreKind::Xray => {
            let output = Command::new(path).arg("version").output();
            output
                .ok()
                .map(|item| String::from_utf8_lossy(&item.stdout).trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "xray".to_string())
        }
        CoreKind::SingBox => {
            let output = Command::new(path).arg("version").output();
            output
                .ok()
                .map(|item| {
                    let text = format!(
                        "{}{}",
                        String::from_utf8_lossy(&item.stdout),
                        String::from_utf8_lossy(&item.stderr)
                    );
                    text.lines().next().unwrap_or("sing-box").trim().to_string()
                })
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "sing-box".to_string())
        }
    };
    Ok((kind, version))
}

fn default_core_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("ORBIT_PROXY_CORE") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(path) = core_installer::managed_xray_path() {
        candidates.push(path);
    }
    if let Some(path) = std::env::var_os("ORBIT_XRAY_PATH") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("ORBIT_SINGBOX_PATH") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(data_dir) = dirs::data_dir() {
        let runtime = data_dir.join("com.orbit.browser").join("runtime").join("proxy-core");
        candidates.push(runtime.join("xray"));
        candidates.push(runtime.join("xray.exe"));
        candidates.push(runtime.join("sing-box"));
        candidates.push(runtime.join("sing-box.exe"));
    }
    for binary in ["xray", "xray.exe", "sing-box", "sing-box.exe", "singbox"] {
        if let Some(path) = find_on_path(binary) {
            candidates.push(path);
        }
    }
    candidates
}

fn find_on_path(binary: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn process_alive(child: &mut Child) -> bool {
    match child.try_wait() {
        Ok(None) => true,
        _ => false,
    }
}


fn wait_for_port(port: u16, timeout: Duration) -> AppResult<()> {
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(AppError::new(
        "proxy_core_start_failed",
        format!("Local proxy core did not open port {port} in time"),
    )
    .retryable(true))
}

fn configure_core_process(command: &mut Command) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = command;
}
