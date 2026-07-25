use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ProxyConfig {
    pub kind: ProxyKind,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub bypass_list: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProxyKind {
    None,
    Http,
    Https,
    Socks4,
    Socks5,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            kind: ProxyKind::None,
            host: None,
            port: None,
            username: None,
            password: None,
            bypass_list: Vec::new(),
        }
    }
}

impl ProxyConfig {
    pub fn chrome_server(&self) -> Option<String> {
        if self.kind == ProxyKind::None {
            return None;
        }
        let host = self.host.as_ref()?;
        let port = self.port?;
        let scheme = match self.kind {
            ProxyKind::None => return None,
            ProxyKind::Http => "http",
            ProxyKind::Https => "https",
            ProxyKind::Socks4 => "socks4",
            ProxyKind::Socks5 => "socks5",
        };
        Some(format!("{scheme}://{host}:{port}"))
    }

    pub fn is_subscription_binding(&self) -> bool {
        self.username
            .as_deref()
            .is_some_and(|value| value.starts_with("orbit-node:"))
    }

    pub fn has_auth(&self) -> bool {
        // 订阅节点绑定标记不是代理账号，避免被当成 proxy-auth extension。
        if self.is_subscription_binding() {
            return false;
        }
        self.username
            .as_deref()
            .is_some_and(|value| !value.is_empty())
            || self
                .password
                .as_deref()
                .is_some_and(|value| !value.is_empty())
    }

    pub fn bound_subscription_node_id(&self) -> Option<String> {
        self.username
            .as_deref()
            .and_then(|value| value.strip_prefix("orbit-node:"))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }
}
