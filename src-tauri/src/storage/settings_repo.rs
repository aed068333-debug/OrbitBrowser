use crate::domain::settings::{SaveSettingsInput, Settings};
use crate::errors::AppResult;
use crate::storage::db::Db;
use chrono::Utc;
use rusqlite::params;

pub fn get(db: &Db) -> AppResult<Settings> {
    let conn = db.connect()?;
    let settings = conn.query_row(
        r#"
        SELECT chrome_path, camoufox_python_path, proxy_core_path, default_concurrency, default_locale,
               default_timezone_id,
               default_viewport_width, default_viewport_height, data_dir,
               aigc_base_url, aigc_model, aigc_api_key, created_at, updated_at
        FROM settings
        WHERE id = 1
        "#,
        [],
        |row| {
            Ok(Settings {
                chrome_path: normalize_optional(row.get(0)?),
                camoufox_python_path: normalize_optional(row.get(1)?),
                proxy_core_path: normalize_optional(row.get(2)?),
                default_concurrency: row.get(3)?,
                default_locale: row.get(4)?,
                default_timezone_id: row.get(5)?,
                default_viewport_width: row.get(6)?,
                default_viewport_height: row.get(7)?,
                data_dir: row.get(8)?,
                aigc_base_url: row.get(9)?,
                aigc_model: row.get(10)?,
                aigc_api_key: row.get(11)?,
                created_at: row.get(12)?,
                updated_at: row.get(13)?,
            })
        },
    )?;
    Ok(settings)
}

pub fn save(db: &Db, input: SaveSettingsInput) -> AppResult<Settings> {
    let conn = db.connect()?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        r#"
        UPDATE settings
        SET chrome_path = ?1,
            camoufox_python_path = ?2,
            proxy_core_path = ?3,
            default_concurrency = ?4,
            default_locale = ?5,
            default_timezone_id = ?6,
            default_viewport_width = ?7,
            default_viewport_height = ?8,
            aigc_base_url = ?9,
            aigc_model = ?10,
            aigc_api_key = ?11,
            updated_at = ?12
        WHERE id = 1
        "#,
        params![
            normalize_optional(input.chrome_path),
            normalize_optional(input.camoufox_python_path),
            normalize_optional(input.proxy_core_path),
            input.default_concurrency.max(1),
            input.default_locale,
            input.default_timezone_id,
            input.default_viewport_width.max(320),
            input.default_viewport_height.max(240),
            normalize_optional(input.aigc_base_url),
            normalize_optional(input.aigc_model),
            normalize_optional(input.aigc_api_key),
            now
        ],
    )?;
    get(db)
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|item| {
        let trimmed = item.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_db() -> AppResult<(Db, PathBuf)> {
        let data_dir =
            std::env::temp_dir().join(format!("orbit-settings-repo-test-{}", uuid::Uuid::new_v4()));
        let db = Db::initialize(data_dir.clone())?;
        Ok((db, data_dir))
    }

    #[test]
    fn save_persists_normalized_camoufox_python_path() -> AppResult<()> {
        let (db, data_dir) = test_db()?;
        let persisted = save(
            &db,
            SaveSettingsInput {
                chrome_path: None,
                camoufox_python_path: Some("  /opt/orbit/bin/python3  ".to_string()),
                proxy_core_path: None,
                default_concurrency: 2,
                default_locale: "zh-CN".to_string(),
                default_timezone_id: "auto".to_string(),
                default_viewport_width: 1280,
                default_viewport_height: 800,
                aigc_base_url: None,
                aigc_model: None,
                aigc_api_key: None,
            },
        )?;
        assert_eq!(
            persisted.camoufox_python_path.as_deref(),
            Some("/opt/orbit/bin/python3")
        );

        drop(db);
        let db = Db::initialize(data_dir.clone())?;
        assert_eq!(
            get(&db)?.camoufox_python_path.as_deref(),
            Some("/opt/orbit/bin/python3")
        );

        let cleared = save(
            &db,
            SaveSettingsInput {
                chrome_path: None,
                camoufox_python_path: Some("   ".to_string()),
                proxy_core_path: None,
                default_concurrency: 2,
                default_locale: "zh-CN".to_string(),
                default_timezone_id: "auto".to_string(),
                default_viewport_width: 1280,
                default_viewport_height: 800,
                aigc_base_url: None,
                aigc_model: None,
                aigc_api_key: None,
            },
        )?;
        assert_eq!(cleared.camoufox_python_path, None);
        drop(db);
        let _ = std::fs::remove_dir_all(data_dir);
        Ok(())
    }
}

