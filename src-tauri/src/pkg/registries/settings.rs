//! Settings registry — declarative per-package settings.
//!
//! A package's manifest declares a flat `settings.schema` (key/type/default/
//! label). The registry holds the schema in memory keyed by `pkg_id` so the
//! kernel snapshot and the `pkg_settings_get` command can merge declared
//! defaults with actual user-set rows.
//!
//! Defaults are NOT seeded into `pkg_settings` at register time — the
//! `pkg_installed` row hasn't been written yet (kernel walks registries
//! first, then persists), and the `pkg_settings.pkg_id` foreign key blocks
//! any insert. Instead, `pkg_settings_get` synthesizes defaults from the
//! schema for any key that has no row.
//!
//! Reads / writes of values themselves go through the `pkg_settings_*` Tauri
//! commands directly against `pkg_settings` — the registry doesn't proxy them.
//! Storage is schemaless JSON (`value_json`) so any field type round-trips.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::commands::db::PaDb;
use crate::pkg::manifest::{Package, SettingsField};
use crate::pkg::registry::Registry;

pub struct SettingsRegistry {
    db: Arc<PaDb>,
    /// `pkg_id` → declared field list (not values). Cleared on uninstall;
    /// FK cascade on `pkg_installed` drops the value rows.
    schemas: RwLock<HashMap<String, Vec<SettingsField>>>,
}

impl SettingsRegistry {
    pub fn new(db: Arc<PaDb>) -> Self {
        Self {
            db,
            schemas: RwLock::new(HashMap::new()),
        }
    }

    /// Read the declared schema for a package (used by snapshot + the
    /// `pkg_settings_get` command's "fall back to default" path).
    pub fn schema_for(&self, pkg_id: &str) -> Option<Vec<SettingsField>> {
        self.schemas.read().ok()?.get(pkg_id).cloned()
    }
}

impl Registry for SettingsRegistry {
    fn name(&self) -> &'static str {
        "settings"
    }

    fn register(&self, pkg: &Package) -> Result<()> {
        // Same rule the daemon's pkg index applies to the same manifest.
        let Some(schema) = crate::pkg::settings_values::declared_schema(pkg) else {
            return Ok(());
        };
        let mut map = self
            .schemas
            .write()
            .map_err(|_| anyhow!("settings registry lock poisoned"))?;
        map.insert(pkg.manifest.id.clone(), schema);
        Ok(())
    }

    fn unregister(&self, pkg_id: &str) -> Result<()> {
        if let Ok(mut map) = self.schemas.write() {
            map.remove(pkg_id);
        }
        // Explicit DELETE — SQLite FKs are OFF in this DB so the cascade on
        // `pkg_installed` won't actually fire. Clearing values here also gives
        // a clean reinstall (defaults re-seed from scratch).
        let db = self.db.clone();
        let id = pkg_id.to_string();
        let _ = tauri::async_runtime::block_on(async move {
            let pool = db.ensure_pool().await.map_err(|e| anyhow!(e))?;
            sqlx::query("DELETE FROM pkg_settings WHERE pkg_id = ?")
                .bind(&id)
                .execute(&pool)
                .await
                .map_err(|e| anyhow!("delete pkg_settings: {e}"))?;
            Ok::<_, anyhow::Error>(())
        });
        Ok(())
    }

    fn snapshot(&self) -> Value {
        let map = match self.schemas.read() {
            Ok(g) => g,
            Err(_) => return json!({ "error": "lock poisoned" }),
        };
        // Pull current values per pkg in one shot. Best-effort: a DB error
        // surfaces the schema without values rather than failing the whole
        // status call.
        let pkg_ids: Vec<String> = map.keys().cloned().collect();
        let values_map = self.values_snapshot(&pkg_ids).unwrap_or_default();
        let entries: Vec<Value> = map
            .iter()
            .map(|(pkg_id, schema)| {
                let values = values_map.get(pkg_id).cloned().unwrap_or_else(|| json!({}));
                json!({
                    "pkg_id": pkg_id,
                    "schema": schema,
                    "values": values,
                })
            })
            .collect();
        json!({ "count": entries.len(), "entries": entries })
    }
}

impl SettingsRegistry {
    /// Read all `pkg_settings` rows for the given pkg_ids in a single round-
    /// trip. Returned as `{pkg_id: {key: parsed_value, ...}}`.
    fn values_snapshot(&self, pkg_ids: &[String]) -> Result<HashMap<String, Value>> {
        if pkg_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let db = self.db.clone();
        let ids = pkg_ids.to_vec();
        tauri::async_runtime::block_on(async move {
            let pool = db.ensure_pool().await.map_err(|e| anyhow!(e))?;
            let mut out: HashMap<String, Value> = HashMap::new();
            for id in ids {
                let rows: Vec<(String, String)> =
                    sqlx::query_as("SELECT key, value_json FROM pkg_settings WHERE pkg_id = ?")
                        .bind(&id)
                        .fetch_all(&pool)
                        .await
                        .map_err(|e| anyhow!("read pkg_settings: {e}"))?;
                let mut obj = serde_json::Map::new();
                for (k, vj) in rows {
                    let v = serde_json::from_str(&vj).unwrap_or(Value::String(vj));
                    obj.insert(k, v);
                }
                out.insert(id, Value::Object(obj));
            }
            Ok::<_, anyhow::Error>(out)
        })
    }
}
