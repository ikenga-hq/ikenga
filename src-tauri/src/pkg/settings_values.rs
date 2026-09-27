//! Per-package settings values: the `pkg_settings` table read/upsert and the
//! schema-default merge behind `pkg_settings_get` / `pkg_settings_set`.
//!
//! Headless (WP-19): the desktop commands pass the schema from the in-memory
//! `SettingsRegistry`; the daemon passes the schema from the manifest in its
//! read-only `--pkgs-dir` index. Both registries take the schema from the
//! manifest through [`declared_schema`], so for the same manifest they hold
//! the same fields — and an unknown pkg is `None` on both, which is the
//! desktop's answer too (`schema: null`, stored rows only — not an error).

use serde_json::{Map, Value};

use crate::db::PaDb;
use crate::pkg::manifest::{Package, SettingsField};

#[derive(serde::Serialize)]
pub struct PkgSettingsSnapshot {
    pub pkg_id: String,
    pub schema: Value,
    pub values: Value,
}

/// The schema a registry holds for `pkg`: `settings.schema` when it declares
/// at least one field, else `None` (a pkg with an empty or absent block has no
/// schema to report — the `SettingsRegistry` never records one).
pub fn declared_schema(pkg: &Package) -> Option<Vec<SettingsField>> {
    match &pkg.manifest.settings {
        Some(block) if !block.schema.is_empty() => Some(block.schema.clone()),
        _ => None,
    }
}

/// Stored rows for `pkg_id` merged over `schema`'s defaults.
///
/// Merge: schema defaults provide the baseline, stored rows override. Lets
/// `pkg_settings_get` return a complete shape from first-launch even before
/// the user has set anything (the registry doesn't pre-seed because the
/// pkg_settings.pkg_id FK isn't satisfied until kernel persists install).
pub async fn get(
    db: &PaDb,
    pkg_id: String,
    schema: Option<Vec<SettingsField>>,
) -> Result<PkgSettingsSnapshot, String> {
    let pool = db.ensure_pool().await?;
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value_json FROM pkg_settings WHERE pkg_id = ?")
            .bind(&pkg_id)
            .fetch_all(&pool)
            .await
            .map_err(|e| format!("read pkg_settings: {e}"))?;
    let mut stored = Map::new();
    for (k, vj) in rows {
        let v: Value = serde_json::from_str(&vj).unwrap_or(Value::String(vj));
        stored.insert(k, v);
    }

    let mut merged = Map::new();
    if let Some(fields) = &schema {
        for f in fields {
            merged.insert(f.key.clone(), f.default.clone());
        }
    }
    for (k, v) in stored {
        merged.insert(k, v);
    }

    Ok(PkgSettingsSnapshot {
        pkg_id,
        schema: serde_json::to_value(schema).unwrap_or(Value::Null),
        values: Value::Object(merged),
    })
}

/// Upsert one `(pkg_id, key)` row. Storage is schemaless JSON (`value_json`),
/// so any value round-trips.
pub async fn set(db: &PaDb, pkg_id: &str, key: &str, value: &Value) -> Result<(), String> {
    let value_json = serde_json::to_string(value).map_err(|e| format!("serialize value: {e}"))?;
    let now = chrono::Utc::now().timestamp_millis();
    let pool = db.ensure_pool().await?;
    sqlx::query(
        "INSERT INTO pkg_settings (pkg_id, key, value_json, updated_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(pkg_id, key) DO UPDATE SET
           value_json = excluded.value_json,
           updated_at = excluded.updated_at",
    )
    .bind(pkg_id)
    .bind(key)
    .bind(&value_json)
    .bind(now)
    .execute(&pool)
    .await
    .map_err(|e| format!("upsert pkg_settings: {e}"))?;
    Ok(())
}
