use crate::domain::environment::Environment;
use crate::domain::proxy::{ProxyConfig, ProxyKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionNodeProtocol {
    Http,
    Https,
    Socks4,
    Socks5,
    Shadowsocks,
    Trojan,
    Vmess,
    Vless,
    Hysteria2,
    Unknown,
}

impl Default for SubscriptionNodeProtocol {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ProxySubscription {
    pub id: String,
    pub name: String,
    pub url: String,
    pub enabled: bool,
    pub auto_sync_interval_minutes: i64,
    pub last_synced_at: Option<String>,
    pub last_error: Option<String>,
    pub node_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ProxySubscriptionNode {
    pub id: String,
    pub subscription_id: String,
    pub name: String,
    pub protocol: SubscriptionNodeProtocol,
    pub server: String,
    pub port: u16,
    pub share_uri: String,
    pub tags: Vec<String>,
    pub latency_ms: Option<i64>,
    pub is_meta: bool,
    pub config_json: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SaveProxySubscriptionInput {
    pub id: Option<String>,
    pub name: String,
    pub url: String,
    pub enabled: bool,
    pub auto_sync_interval_minutes: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ProxySubscriptionSyncResult {
    pub subscription: ProxySubscription,
    pub nodes: Vec<ProxySubscriptionNode>,
    pub imported: i64,
    pub skipped: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ApplySubscriptionNodeInput {
    pub environment_id: String,
    pub node_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CreateEnvironmentsFromNodesInput {
    pub subscription_id: String,
    pub node_ids: Option<Vec<String>>,
    pub group_id: Option<String>,
    pub name_prefix: Option<String>,
    pub only_usable: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CreateEnvironmentsFromNodesResult {
    pub created: Vec<Environment>,
    pub skipped: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ProbeSubscriptionNodesInput {
    pub subscription_id: String,
    pub node_ids: Option<Vec<String>>,
    pub only_usable: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SubscriptionNodeLatency {
    pub node_id: String,
    pub name: String,
    pub ok: bool,
    pub latency_ms: Option<i64>,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ProbeSubscriptionNodesResult {
    pub results: Vec<SubscriptionNodeLatency>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ProxyCoreStatus {
    pub found: bool,
    pub path: Option<String>,
    pub kind: Option<String>,
    pub version: Option<String>,
    pub searched_paths: Vec<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct LocalProxyEndpoint {
    pub host: String,
    pub port: u16,
    pub kind: ProxyKind,
    pub node_id: String,
    pub node_name: String,
    pub core_kind: String,
    pub config_path: String,
}

impl LocalProxyEndpoint {
    #[allow(dead_code)]
    pub fn to_proxy_config(&self, bypass_list: Vec<String>) -> ProxyConfig {
        ProxyConfig {
            kind: self.kind.clone(),
            host: Some(self.host.clone()),
            port: Some(self.port),
            username: None,
            password: None,
            bypass_list,
        }
    }
}
