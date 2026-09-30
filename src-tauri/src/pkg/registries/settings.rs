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
        let fut = async move {
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
        };
        block_on_from_any_context(fut)
    }
}

/// Run `fut` to completion from sync code that may itself be running on a
/// tokio worker.
///
/// `snapshot()` is sync (the `Registry` trait), but `Kernel::status()` — which
/// calls it — is reached from async code too (the `ngwa_snapshot` command and
/// `GET /iyke/ngwa/snapshot`). A bare `tauri::async_runtime::block_on` there
/// panics with "Cannot start a runtime from within a runtime", which is what
/// left the Ngwa Installed tab scanning forever on v0.18.4 as soon as any
/// installed pkg declared a settings schema.
///
/// - No runtime on this thread: block on tauri's runtime, as before.
/// - Multi-thread runtime: `block_in_place` hands this worker's tasks off and
///   blocks here safely.
/// - Current-thread runtime: blocking would deadlock or panic, so return an
///   empty map; the snapshot then carries the schema without values (the same
///   best-effort the caller already applies to a DB error).
fn block_on_from_any_context<F>(fut: F) -> Result<HashMap<String, Value>>
where
    F: std::future::Future<Output = Result<HashMap<String, Value>>> + Send,
{
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current() {
        Err(_) => tauri::async_runtime::block_on(fut),
        Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| handle.block_on(fut))
        }
        Ok(_) => {
            log::warn!(
                "[pkg.settings] snapshot called on a current-thread runtime; returning schema without values"
            );
            Ok(HashMap::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_with_schema(db: Arc<PaDb>) -> SettingsRegistry {
        let reg = SettingsRegistry::new(db);
        let field: SettingsField = serde_json::from_value(json!({
            "key": "language",
            "type": "string",
            "label": "Language",
            "default": "en"
        }))
        .expect("settings field");
        reg.schemas
            .write()
            .unwrap()
            .insert("com.test.meetings".into(), vec![field]);
        reg
    }

    async fn seed_value(db: &Arc<PaDb>) {
        let pool = db.ensure_pool().await.expect("pool");
        // No `pkg_installed` parent row in this fixture; the pool is a single
        // connection, so turning FKs off here sticks for the seed insert.
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&pool)
            .await
            .expect("fk off");
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS pkg_settings (pkg_id TEXT NOT NULL, key TEXT NOT NULL,              value_json TEXT NOT NULL, updated_at INTEGER NOT NULL, PRIMARY KEY (pkg_id, key))",
        )
        .execute(&pool)
        .await
        .expect("create pkg_settings");
        sqlx::query(
            "INSERT INTO pkg_settings (pkg_id, key, value_json, updated_at) VALUES (?, ?, ?, 0)",
        )
        .bind("com.test.meetings")
        .bind("language")
        .bind("\"yo\"")
        .execute(&pool)
        .await
        .expect("seed pkg_settings");
    }

    fn language_value(snap: &Value) -> Option<Value> {
        snap["entries"]
            .as_array()?
            .iter()
            .find(|e| e["pkg_id"] == "com.test.meetings")
            .and_then(|e| e["values"].get("language").cloned())
    }

    /// Regression (v0.18.4 Installed-scan hang): `snapshot()` reached from a
    /// tokio worker — as `ngwa_snapshot` does via `Kernel::status()` — must not
    /// panic, and must still read the stored values.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn snapshot_from_multi_thread_runtime_reads_values_without_panicking() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Arc::new(PaDb::new(tmp.path().join("ikenga.db")));
        seed_value(&db).await;
        let reg = registry_with_schema(db);
        let snap = reg.snapshot();
        assert_eq!(language_value(&snap), Some(json!("yo")), "snapshot: {snap}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn snapshot_from_current_thread_runtime_degrades_instead_of_panicking() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Arc::new(PaDb::new(tmp.path().join("ikenga.db")));
        let reg = registry_with_schema(db);
        let snap = reg.snapshot();
        assert_eq!(snap["count"], 1, "schema still listed: {snap}");
    }

    #[test]
    fn snapshot_outside_any_runtime_reads_values() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Arc::new(PaDb::new(tmp.path().join("ikenga.db")));
        tauri::async_runtime::block_on(seed_value(&db));
        let reg = registry_with_schema(db);
        let snap = reg.snapshot();
        assert_eq!(language_value(&snap), Some(json!("yo")), "snapshot: {snap}");
    }
}
