use crate::domain::subscription::SubscriptionNodeProtocol;
use crate::errors::{AppError, AppResult};
use base64::Engine;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct ParsedSubscriptionNode {
    pub name: String,
    pub protocol: SubscriptionNodeProtocol,
    pub server: String,
    pub port: u16,
    pub share_uri: String,
    pub tags: Vec<String>,
    pub is_meta: bool,
    pub config: Value,
}

pub fn decode_subscription_body(body: &[u8]) -> AppResult<String> {
    let trimmed = trim_bytes(body);
    if trimmed.is_empty() {
        return Err(AppError::new(
            "subscription_empty",
            "Subscription body is empty",
        ));
    }

    if looks_like_share_list(trimmed) {
        return Ok(String::from_utf8_lossy(trimmed).into_owned());
    }

    let compact = std::str::from_utf8(trimmed)
        .unwrap_or_default()
        .split_whitespace()
        .collect::<String>();
    if !compact.is_empty() {
        if let Ok(decoded) = decode_base64(&compact) {
            if looks_like_share_list(decoded.as_bytes()) || decoded.contains("://") {
                return Ok(decoded);
            }
        }
    }

    if let Ok(decoded) = decode_base64_bytes(trimmed) {
        if looks_like_share_list(&decoded) || String::from_utf8_lossy(&decoded).contains("://") {
            return Ok(String::from_utf8_lossy(&decoded).into_owned());
        }
    }

    Ok(String::from_utf8_lossy(trimmed).into_owned())
}

pub fn parse_subscription_content(content: &str) -> AppResult<Vec<ParsedSubscriptionNode>> {
    let mut nodes = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        match parse_share_uri(line) {
            Ok(node) => nodes.push(node),
            Err(err) => {
                tracing::debug!(uri = %line, error = %err, "skip unsupported subscription line");
            }
        }
    }

    if nodes.is_empty() {
        return Err(AppError::new(
            "subscription_no_nodes",
            "No supported proxy nodes found in subscription",
        ));
    }
    Ok(nodes)
}

pub fn parse_share_uri(raw: &str) -> AppResult<ParsedSubscriptionNode> {
    let uri = raw.trim();
    if uri.is_empty() {
        return Err(AppError::new("subscription_invalid", "Empty share URI"));
    }

    let (scheme, rest) = uri
        .split_once("://")
        .ok_or_else(|| AppError::new("subscription_invalid", "Share URI missing scheme"))?;
    let scheme = scheme.to_ascii_lowercase();

    match scheme.as_str() {
        "http" | "https" | "socks" | "socks4" | "socks5" => parse_standard_proxy_uri(uri, &scheme),
        "ss" => parse_shadowsocks_uri(uri, rest),
        "trojan" => parse_trojan_uri(uri, rest),
        "vmess" => parse_vmess_uri(uri, rest),
        "vless" => parse_vless_uri(uri, rest),
        "hysteria2" | "hy2" => parse_hysteria2_uri(uri, rest),
        _ => Err(AppError::new(
            "subscription_unsupported",
            format!("Unsupported share scheme: {scheme}"),
        )),
    }
}

fn parse_standard_proxy_uri(uri: &str, scheme: &str) -> AppResult<ParsedSubscriptionNode> {
    let (main, fragment) = split_fragment(uri);
    let (scheme_part, rest) = main
        .split_once("://")
        .ok_or_else(|| AppError::new("subscription_invalid", "Invalid proxy URI"))?;
    let _ = scheme_part;
    let (auth_and_host, _) = split_query(rest);
    let (username, password, hostport) = if let Some((userinfo, hostport)) = auth_and_host.split_once('@') {
        if let Some((username, password)) = userinfo.split_once(':') {
            (Some(percent_decode(username)), Some(percent_decode(password)), hostport)
        } else {
            (Some(percent_decode(userinfo)), None, hostport)
        }
    } else {
        (None, None, auth_and_host)
    };
    let (host, port) = split_host_port(hostport)?;
    let name = fragment_name(fragment).unwrap_or_else(|| format!("{host}:{port}"));
    let protocol = match scheme {
        "https" => SubscriptionNodeProtocol::Https,
        "socks4" => SubscriptionNodeProtocol::Socks4,
        "socks" | "socks5" => SubscriptionNodeProtocol::Socks5,
        _ => SubscriptionNodeProtocol::Http,
    };
    let mut config = Map::new();
    config.insert("kind".into(), json!(scheme_to_proxy_kind(scheme)));
    config.insert("host".into(), json!(host));
    config.insert("port".into(), json!(port));
    if let Some(username) = username.filter(|value| !value.is_empty()) {
        config.insert("username".into(), json!(username));
    }
    if let Some(password) = password.filter(|value| !value.is_empty()) {
        config.insert("password".into(), json!(password));
    }

    Ok(ParsedSubscriptionNode {
        is_meta: is_meta_name(&name),
        tags: infer_tags(&name, &host),
        name,
        protocol,
        server: host.clone(),
        port,
        share_uri: uri.to_string(),
        config: Value::Object(config),
    })
}

fn parse_shadowsocks_uri(uri: &str, rest: &str) -> AppResult<ParsedSubscriptionNode> {
    let (main, fragment) = split_fragment(rest);
    let name = fragment_name(fragment).unwrap_or_else(|| "Shadowsocks".to_string());

    let (method, password, host, port) = if let Some((userinfo, hostport)) = main.split_once('@') {
        let decoded = if userinfo.contains(':') {
            userinfo.to_string()
        } else {
            decode_base64(userinfo).unwrap_or_else(|_| userinfo.to_string())
        };
        let (method, password) = decoded
            .split_once(':')
            .ok_or_else(|| AppError::new("subscription_invalid", "Invalid Shadowsocks userinfo"))?;
        let (host, port) = split_host_port(hostport)?;
        (method.to_string(), password.to_string(), host, port)
    } else {
        let decoded = decode_base64(main)
            .map_err(|err| AppError::new("subscription_invalid", format!("Invalid ss payload: {err}")))?;
        let (userinfo, hostport) = decoded
            .split_once('@')
            .ok_or_else(|| AppError::new("subscription_invalid", "Invalid ss decoded payload"))?;
        let (method, password) = userinfo
            .split_once(':')
            .ok_or_else(|| AppError::new("subscription_invalid", "Invalid ss credentials"))?;
        let (host, port) = split_host_port(hostport)?;
        (method.to_string(), password.to_string(), host, port)
    };

    let mut config = Map::new();
    config.insert("protocol".into(), json!("shadowsocks"));
    config.insert("server".into(), json!(host));
    config.insert("port".into(), json!(port));
    config.insert("method".into(), json!(method));
    config.insert("password".into(), json!(password));

    Ok(ParsedSubscriptionNode {
        is_meta: is_meta_name(&name),
        tags: infer_tags(&name, &host),
        name,
        protocol: SubscriptionNodeProtocol::Shadowsocks,
        server: host,
        port,
        share_uri: uri.to_string(),
        config: Value::Object(config),
    })
}

fn parse_trojan_uri(uri: &str, rest: &str) -> AppResult<ParsedSubscriptionNode> {
    let (main, fragment) = split_fragment(rest);
    let name = fragment_name(fragment).unwrap_or_else(|| "Trojan".to_string());
    let (userinfo, host_and_query) = main
        .split_once('@')
        .ok_or_else(|| AppError::new("subscription_invalid", "Invalid trojan URI"))?;
    let (hostport, query) = split_query(host_and_query);
    let (host, port) = split_host_port(hostport)?;
    let params = parse_query(query);
    let password = percent_decode(userinfo);

    let mut config = Map::new();
    config.insert("protocol".into(), json!("trojan"));
    config.insert("server".into(), json!(host));
    config.insert("port".into(), json!(port));
    config.insert("password".into(), json!(password));
    for key in ["sni", "peer", "type", "host", "path", "security", "alpn", "fp", "allowInsecure", "insecure"] {
        insert_param(&mut config, &params, key);
    }

    Ok(ParsedSubscriptionNode {
        is_meta: is_meta_name(&name),
        tags: infer_tags(&name, &host),
        name,
        protocol: SubscriptionNodeProtocol::Trojan,
        server: host,
        port,
        share_uri: uri.to_string(),
        config: Value::Object(config),
    })
}

fn parse_vmess_uri(uri: &str, rest: &str) -> AppResult<ParsedSubscriptionNode> {
    let payload = rest.trim();
    let decoded = decode_base64(payload)
        .map_err(|err| AppError::new("subscription_invalid", format!("Invalid vmess payload: {err}")))?;
    let value: Value = serde_json::from_str(&decoded)
        .map_err(|err| AppError::new("subscription_invalid", format!("Invalid vmess JSON: {err}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| AppError::new("subscription_invalid", "vmess payload must be an object"))?;
    let host = object
        .get("add")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let port = object
        .get("port")
        .and_then(|value| match value {
            Value::Number(number) => number.as_u64().map(|item| item as u16),
            Value::String(text) => text.parse().ok(),
            _ => None,
        })
        .ok_or_else(|| AppError::new("subscription_invalid", "vmess port is missing"))?;
    let name = object
        .get("ps")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("{host}:{port}"));

    let mut config = object.clone();
    config.insert("protocol".into(), json!("vmess"));
    config.insert("server".into(), json!(host.clone()));
    config.insert("port".into(), json!(port));

    Ok(ParsedSubscriptionNode {
        is_meta: is_meta_name(&name),
        tags: infer_tags(&name, &host),
        name,
        protocol: SubscriptionNodeProtocol::Vmess,
        server: host,
        port,
        share_uri: uri.to_string(),
        config: Value::Object(config),
    })
}

fn parse_vless_uri(uri: &str, rest: &str) -> AppResult<ParsedSubscriptionNode> {
    let (main, fragment) = split_fragment(rest);
    let name = fragment_name(fragment).unwrap_or_else(|| "VLESS".to_string());
    let (userinfo, host_and_query) = main
        .split_once('@')
        .ok_or_else(|| AppError::new("subscription_invalid", "Invalid vless URI"))?;
    let (hostport, query) = split_query(host_and_query);
    let (host, port) = split_host_port(hostport)?;
    let params = parse_query(query);
    let uuid = percent_decode(userinfo);

    let mut config = Map::new();
    config.insert("protocol".into(), json!("vless"));
    config.insert("server".into(), json!(host));
    config.insert("port".into(), json!(port));
    config.insert("uuid".into(), json!(uuid));
    config.insert(
        "encryption".into(),
        json!(params
            .get("encryption")
            .cloned()
            .unwrap_or_else(|| "none".into())),
    );
    for key in [
        "type",
        "security",
        "flow",
        "fp",
        "sni",
        "host",
        "path",
        "serviceName",
        "mode",
        "headerType",
        "alpn",
        "pbk",
        "sid",
        "spx",
        "pqv",
        "insecure",
        "allowInsecure",
    ] {
        insert_param(&mut config, &params, key);
    }

    Ok(ParsedSubscriptionNode {
        is_meta: is_meta_name(&name),
        tags: infer_tags(&name, &host),
        name,
        protocol: SubscriptionNodeProtocol::Vless,
        server: host,
        port,
        share_uri: uri.to_string(),
        config: Value::Object(config),
    })
}

fn parse_hysteria2_uri(uri: &str, rest: &str) -> AppResult<ParsedSubscriptionNode> {
    let (main, fragment) = split_fragment(rest);
    let name = fragment_name(fragment).unwrap_or_else(|| "Hysteria2".to_string());
    let (userinfo, host_and_query) = if let Some((userinfo, host_and_query)) = main.split_once('@') {
        (Some(userinfo), host_and_query)
    } else {
        (None, main)
    };
    let (hostport, query) = split_query(host_and_query);
    let (host, port) = split_host_port(hostport)?;
    let params = parse_query(query);
    let password = userinfo
        .map(percent_decode)
        .or_else(|| params.get("auth").cloned())
        .unwrap_or_default();

    let mut config = Map::new();
    config.insert("protocol".into(), json!("hysteria2"));
    config.insert("server".into(), json!(host));
    config.insert("port".into(), json!(port));
    config.insert("password".into(), json!(password));
    for key in ["sni", "insecure", "obfs", "obfs-password"] {
        insert_param(&mut config, &params, key);
    }

    Ok(ParsedSubscriptionNode {
        is_meta: is_meta_name(&name),
        tags: infer_tags(&name, &host),
        name,
        protocol: SubscriptionNodeProtocol::Hysteria2,
        server: host,
        port,
        share_uri: uri.to_string(),
        config: Value::Object(config),
    })
}

fn scheme_to_proxy_kind(scheme: &str) -> &'static str {
    match scheme {
        "https" => "https",
        "socks4" => "socks4",
        "socks" | "socks5" => "socks5",
        _ => "http",
    }
}

fn is_meta_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "剩余",
        "流量",
        "到期",
        "重置",
        "套餐",
        "expire",
        "traffic",
        "reset",
    ]
    .iter()
    .any(|token| name.contains(token) || lower.contains(token))
}

fn infer_tags(name: &str, host: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let source = format!("{name} {host}").to_ascii_lowercase();
    let mapping = [
        ("日本", "jp"),
        ("东京", "jp"),
        ("japan", "jp"),
        ("tokyo", "jp"),
        ("新加坡", "sg"),
        ("singapore", "sg"),
        ("韩国", "kr"),
        ("korea", "kr"),
        ("香港", "hk"),
        ("hongkong", "hk"),
        ("台湾", "tw"),
        ("taiwan", "tw"),
        ("美国", "us"),
        ("america", "us"),
        ("usa", "us"),
        ("德国", "de"),
        ("germany", "de"),
        ("英国", "uk"),
        ("london", "uk"),
    ];
    for (needle, tag) in mapping {
        if source.contains(needle) && !tags.iter().any(|item| item == tag) {
            tags.push(tag.to_string());
        }
    }
    tags
}

fn looks_like_share_list(body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    [
        "vless://",
        "vmess://",
        "ss://",
        "trojan://",
        "socks5://",
        "http://",
        "https://",
        "hysteria2://",
        "hy2://",
    ]
    .iter()
    .any(|token| text.contains(token))
}

fn trim_bytes(body: &[u8]) -> &[u8] {
    let start = body
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(body.len());
    let end = body
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map(|index| index + 1)
        .unwrap_or(0);
    if start >= end {
        &[]
    } else {
        &body[start..end]
    }
}

fn decode_base64(input: &str) -> Result<String, String> {
    let bytes = decode_base64_bytes(input.as_bytes())?;
    String::from_utf8(bytes).map_err(|err| err.to_string())
}

fn decode_base64_bytes(input: &[u8]) -> Result<Vec<u8>, String> {
    let cleaned = input
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    base64::engine::general_purpose::STANDARD
        .decode(&cleaned)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(&cleaned))
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&cleaned))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&cleaned))
        .map_err(|err| err.to_string())
}

fn split_fragment(value: &str) -> (&str, Option<&str>) {
    match value.split_once('#') {
        Some((main, fragment)) => (main, Some(fragment)),
        None => (value, None),
    }
}

fn split_query(value: &str) -> (&str, Option<&str>) {
    match value.split_once('?') {
        Some((main, query)) => (main, Some(query)),
        None => (value, None),
    }
}

fn fragment_name(fragment: Option<&str>) -> Option<String> {
    fragment
        .map(percent_decode)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_query(query: Option<&str>) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Some(query) = query else {
        return map;
    };
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(percent_decode(key), percent_decode(value));
    }
    map
}

fn insert_param(config: &mut Map<String, Value>, params: &BTreeMap<String, String>, key: &str) {
    if let Some(value) = params.get(key).filter(|item| !item.is_empty()) {
        config.insert(key.to_string(), json!(value));
    }
}

fn split_host_port(value: &str) -> AppResult<(String, u16)> {
    if let Some(stripped) = value.strip_prefix('[') {
        let (host, rest) = stripped
            .split_once(']')
            .ok_or_else(|| AppError::new("subscription_invalid", "Invalid IPv6 host"))?;
        let port = rest
            .strip_prefix(':')
            .ok_or_else(|| AppError::new("subscription_invalid", "IPv6 host missing port"))?
            .parse()
            .map_err(|_| AppError::new("subscription_invalid", "Invalid port"))?;
        return Ok((host.to_string(), port));
    }
    let (host, port) = value
        .rsplit_once(':')
        .ok_or_else(|| AppError::new("subscription_invalid", "Host missing port"))?;
    let port = port
        .parse()
        .map_err(|_| AppError::new("subscription_invalid", "Invalid port"))?;
    Ok((host.to_string(), port))
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = &value[index + 1..index + 3];
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    output.push(byte);
                    index += 3;
                    continue;
                }
                output.push(bytes[index]);
                index += 1;
            }
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64_vless_subscription() {
        let raw = "dmxlc3M6Ly9mMmEwMjFmYy04MjFhLTRjZmUtYjU5Zi04NzI0ZDk4YTE4ZWRAZHJhZ29ueC1qcDAxLnl1bmpuZXQuY29tOjMyMTAwP3R5cGU9dGNwJmVuY3J5cHRpb249bm9uZSZzZWN1cml0eT10bHMmZmxvdz14dGxzLXJwcngtdmlzaW9uJmZwPWNocm9tZSZpbnNlY3VyZT0xJnNuaT1kMS5hd3NzdGF0aWMuY29tIyVFNiU5NyVBNCVFNiU5QyVBQzAxJTIwQVdT";
        let content = decode_subscription_body(raw.as_bytes()).expect("decode");
        let nodes = parse_subscription_content(&content).expect("parse");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].protocol, SubscriptionNodeProtocol::Vless);
        assert_eq!(nodes[0].server, "dragonx-jp01.yunjnet.com");
        assert_eq!(nodes[0].port, 32100);
        assert!(nodes[0].name.contains("日本") || nodes[0].name.contains("AWS"));
        assert_eq!(nodes[0].config["uuid"], "f2a021fc-821a-4cfe-b59f-8724d98a18ed");
        assert_eq!(nodes[0].config["flow"], "xtls-rprx-vision");
    }

    #[test]
    fn marks_meta_nodes() {
        let node = parse_share_uri(
            "vless://f2a021fc-821a-4cfe-b59f-8724d98a18ed@dragonx-jp01.yunjnet.com:32100?type=tcp&security=tls#%E5%89%A9%E4%BD%99%E6%B5%81%E9%87%8F%EF%BC%9A49.17%20GB",
        )
        .expect("parse");
        assert!(node.is_meta);
    }
}
