CREATE TABLE IF NOT EXISTS proxy_subscriptions (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  url TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  auto_sync_interval_minutes INTEGER NOT NULL DEFAULT 0,
  last_synced_at TEXT,
  last_error TEXT,
  node_count INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS proxy_subscription_nodes (
  id TEXT PRIMARY KEY,
  subscription_id TEXT NOT NULL,
  name TEXT NOT NULL,
  protocol TEXT NOT NULL,
  server TEXT NOT NULL,
  port INTEGER NOT NULL,
  share_uri TEXT NOT NULL,
  tags_json TEXT NOT NULL DEFAULT '[]',
  latency_ms INTEGER,
  is_meta INTEGER NOT NULL DEFAULT 0,
  config_json TEXT NOT NULL DEFAULT '{}',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY(subscription_id) REFERENCES proxy_subscriptions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_proxy_subscription_nodes_subscription
  ON proxy_subscription_nodes(subscription_id);

CREATE INDEX IF NOT EXISTS idx_proxy_subscription_nodes_name
  ON proxy_subscription_nodes(name);
