use crate::domain::subscription::{
    ProxySubscription, ProxySubscriptionNode, SaveProxySubscriptionInput, SubscriptionNodeProtocol,
};
use crate::errors::AppResult;
use crate::proxy::subscription_parser::ParsedSubscriptionNode;
use crate::storage::db::Db;
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

pub fn list_subscriptions(db: &Db) -> AppResult<Vec<ProxySubscription>> {
    let conn = db.connect()?;
    let mut stmt = conn.prepare(
        r#"
        SELECT id, name, url, enabled, auto_sync_interval_minutes, last_synced_at, last_error,
               node_count, created_at, updated_at
        FROM proxy_subscriptions
        ORDER BY updated_at DESC, created_at DESC
        "#,
    )?;
    let rows = stmt
        .query_map([], map_subscription)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_subscription(db: &Db, id: &str) -> AppResult<Option<ProxySubscription>> {
    let conn = db.connect()?;
    let row = conn
        .query_row(
            r#"
            SELECT id, name, url, enabled, auto_sync_interval_minutes, last_synced_at, last_error,
                   node_count, created_at, updated_at
            FROM proxy_subscriptions
            WHERE id = ?1
            "#,
            params![id],
            map_subscription,
        )
        .optional()?;
    Ok(row)
}

pub fn save_subscription(db: &Db, input: SaveProxySubscriptionInput) -> AppResult<ProxySubscription> {
    let conn = db.connect()?;
    let now = Utc::now().to_rfc3339();
    let id = input
        .id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let name = input.name.trim().to_string();
    let url = input.url.trim().to_string();
    if name.is_empty() {
        return Err(crate::errors::AppError::new(
            "validation_error",
            "Subscription name is required",
        ));
    }
    if url.is_empty() {
        return Err(crate::errors::AppError::new(
            "validation_error",
            "Subscription URL is required",
        ));
    }
    let auto_sync_interval_minutes = input.auto_sync_interval_minutes.max(0);

    conn.execute(
        r#"
        INSERT INTO proxy_subscriptions (
          id, name, url, enabled, auto_sync_interval_minutes, last_synced_at, last_error,
          node_count, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, 0, ?6, ?6)
        ON CONFLICT(id) DO UPDATE SET
          name = excluded.name,
          url = excluded.url,
          enabled = excluded.enabled,
          auto_sync_interval_minutes = excluded.auto_sync_interval_minutes,
          updated_at = excluded.updated_at
        "#,
        params![
            id,
            name,
            url,
            if input.enabled { 1 } else { 0 },
            auto_sync_interval_minutes,
            now
        ],
    )?;
    get_subscription(db, &id)?.ok_or_else(|| {
        crate::errors::AppError::new("subscription_not_found", "Subscription was not saved")
    })
}

pub fn delete_subscription(db: &Db, id: &str) -> AppResult<()> {
    let conn = db.connect()?;
    conn.execute(
        "DELETE FROM proxy_subscription_nodes WHERE subscription_id = ?1",
        params![id],
    )?;
    conn.execute("DELETE FROM proxy_subscriptions WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn list_nodes(db: &Db, subscription_id: Option<&str>) -> AppResult<Vec<ProxySubscriptionNode>> {
    let conn = db.connect()?;
    if let Some(subscription_id) = subscription_id {
        let mut stmt = conn.prepare(
            r#"
            SELECT id, subscription_id, name, protocol, server, port, share_uri, tags_json,
                   latency_ms, is_meta, config_json, created_at, updated_at
            FROM proxy_subscription_nodes
            WHERE subscription_id = ?1
            ORDER BY
              CASE WHEN latency_ms IS NULL THEN 1 ELSE 0 END,
              latency_ms ASC,
              is_meta ASC,
              name COLLATE NOCASE ASC
            "#,
        )?;
        let rows = stmt
            .query_map(params![subscription_id], map_node)?
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(rows);
    }

    let mut stmt = conn.prepare(
        r#"
        SELECT id, subscription_id, name, protocol, server, port, share_uri, tags_json,
               latency_ms, is_meta, config_json, created_at, updated_at
        FROM proxy_subscription_nodes
        ORDER BY
          CASE WHEN latency_ms IS NULL THEN 1 ELSE 0 END,
          latency_ms ASC,
          is_meta ASC,
          name COLLATE NOCASE ASC
        "#,
    )?;
    let rows = stmt
        .query_map([], map_node)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_node(db: &Db, id: &str) -> AppResult<Option<ProxySubscriptionNode>> {
    let conn = db.connect()?;
    let row = conn
        .query_row(
            r#"
            SELECT id, subscription_id, name, protocol, server, port, share_uri, tags_json,
                   latency_ms, is_meta, config_json, created_at, updated_at
            FROM proxy_subscription_nodes
            WHERE id = ?1
            "#,
            params![id],
            map_node,
        )
        .optional()?;
    Ok(row)
}

pub fn replace_nodes(
    db: &Db,
    subscription_id: &str,
    nodes: &[ParsedSubscriptionNode],
) -> AppResult<Vec<ProxySubscriptionNode>> {
    let conn = db.connect()?;
    let now = Utc::now().to_rfc3339();
    let tx = conn.unchecked_transaction()?;
    // Preserve latency for same server:port+name if possible.
    let mut previous_latency = std::collections::HashMap::<String, Option<i64>>::new();
    {
        let mut stmt = tx.prepare(
            r#"
            SELECT name, server, port, latency_ms
            FROM proxy_subscription_nodes
            WHERE subscription_id = ?1
            "#,
        )?;
        let rows = stmt.query_map(params![subscription_id], |row| {
            let name: String = row.get(0)?;
            let server: String = row.get(1)?;
            let port: i64 = row.get(2)?;
            let latency: Option<i64> = row.get(3)?;
            Ok((format!("{name}|{server}|{port}"), latency))
        })?;
        for row in rows {
            let (key, latency) = row?;
            previous_latency.insert(key, latency);
        }
    }

    tx.execute(
        "DELETE FROM proxy_subscription_nodes WHERE subscription_id = ?1",
        params![subscription_id],
    )?;

    let mut saved = Vec::with_capacity(nodes.len());
    for node in nodes {
        let id = Uuid::new_v4().to_string();
        let tags_json = serde_json::to_string(&node.tags)?;
        let config_json = serde_json::to_string(&node.config)?;
        let protocol = protocol_to_string(&node.protocol);
        let latency_key = format!("{}|{}|{}", node.name, node.server, node.port);
        let latency_ms = previous_latency.get(&latency_key).copied().flatten();
        tx.execute(
            r#"
            INSERT INTO proxy_subscription_nodes (
              id, subscription_id, name, protocol, server, port, share_uri, tags_json,
              latency_ms, is_meta, config_json, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
            "#,
            params![
                id,
                subscription_id,
                node.name,
                protocol,
                node.server,
                node.port as i64,
                node.share_uri,
                tags_json,
                latency_ms,
                if node.is_meta { 1 } else { 0 },
                config_json,
                now
            ],
        )?;
        saved.push(ProxySubscriptionNode {
            id,
            subscription_id: subscription_id.to_string(),
            name: node.name.clone(),
            protocol: node.protocol.clone(),
            server: node.server.clone(),
            port: node.port,
            share_uri: node.share_uri.clone(),
            tags: node.tags.clone(),
            latency_ms,
            is_meta: node.is_meta,
            config_json: serde_json::to_string(&node.config)?,
            created_at: now.clone(),
            updated_at: now.clone(),
        });
    }

    tx.execute(
        r#"
        UPDATE proxy_subscriptions
        SET node_count = ?1,
            last_synced_at = ?2,
            last_error = NULL,
            updated_at = ?2
        WHERE id = ?3
        "#,
        params![saved.len() as i64, now, subscription_id],
    )?;
    tx.commit()?;
    Ok(saved)
}

pub fn update_node_latency(db: &Db, node_id: &str, latency_ms: Option<i64>) -> AppResult<()> {
    let conn = db.connect()?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        r#"
        UPDATE proxy_subscription_nodes
        SET latency_ms = ?1,
            updated_at = ?2
        WHERE id = ?3
        "#,
        params![latency_ms, now, node_id],
    )?;
    Ok(())
}

pub fn mark_sync_error(db: &Db, subscription_id: &str, error: &str) -> AppResult<()> {
    let conn = db.connect()?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        r#"
        UPDATE proxy_subscriptions
        SET last_error = ?1,
            updated_at = ?2
        WHERE id = ?3
        "#,
        params![error, now, subscription_id],
    )?;
    Ok(())
}

fn map_subscription(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProxySubscription> {
    Ok(ProxySubscription {
        id: row.get(0)?,
        name: row.get(1)?,
        url: row.get(2)?,
        enabled: row.get::<_, i64>(3)? != 0,
        auto_sync_interval_minutes: row.get(4)?,
        last_synced_at: row.get(5)?,
        last_error: row.get(6)?,
        node_count: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn map_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProxySubscriptionNode> {
    let tags_json: String = row.get(7)?;
    let tags = serde_json::from_str(&tags_json).unwrap_or_default();
    let protocol_text: String = row.get(3)?;
    Ok(ProxySubscriptionNode {
        id: row.get(0)?,
        subscription_id: row.get(1)?,
        name: row.get(2)?,
        protocol: protocol_from_string(&protocol_text),
        server: row.get(4)?,
        port: row.get::<_, i64>(5)? as u16,
        share_uri: row.get(6)?,
        tags,
        latency_ms: row.get(8)?,
        is_meta: row.get::<_, i64>(9)? != 0,
        config_json: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn protocol_to_string(protocol: &SubscriptionNodeProtocol) -> &'static str {
    match protocol {
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
}

fn protocol_from_string(value: &str) -> SubscriptionNodeProtocol {
    match value {
        "http" => SubscriptionNodeProtocol::Http,
        "https" => SubscriptionNodeProtocol::Https,
        "socks4" => SubscriptionNodeProtocol::Socks4,
        "socks5" => SubscriptionNodeProtocol::Socks5,
        "shadowsocks" => SubscriptionNodeProtocol::Shadowsocks,
        "trojan" => SubscriptionNodeProtocol::Trojan,
        "vmess" => SubscriptionNodeProtocol::Vmess,
        "vless" => SubscriptionNodeProtocol::Vless,
        "hysteria2" => SubscriptionNodeProtocol::Hysteria2,
        _ => SubscriptionNodeProtocol::Unknown,
    }
}
