//! `/api/rpc` bodies for the local-state commands served in WP-19 slice 2:
//! Supabase config, env-backed secret names, settings, data health, backup
//! list/delete and pkg settings.
//!
//! The arm *names* stay in `rpc.rs`'s dispatch `match` (the parity ratchet
//! reads them there); the arms delegate here. Every body calls the same core
//! the desktop `#[tauri::command]` calls (`server::shared::*`,
//! `pkg::settings_values`), rooted at the daemon's `--data-dir` instead of
//! `app_data_dir`, and returns the same serialized type — so the JSON shapes
//! are the desktop's by construction.
//!
//! **Arguments.** The web transport forwards `tauri-cmd.ts`'s argument object
//! verbatim: camelCase (`projectId`, `anonKey`, `pkgId`), because on desktop
//! Tauri does the camel → snake conversion and nothing does it here. The
//! snake_case spelling is accepted as well, for curl'd probes and callers
//! using the Rust names. `null` reads as absent, which is how Tauri
//! deserializes an `Option<T>` argument.
//!
//! **No `--data-dir`.** Every command here reads or writes something under
//! the data dir, so without one each returns an error naming the missing flag
//! — never an empty answer that would read as authoritative.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::OnceCell;
use tracing::warn;

use super::rpc::RpcResponse;
use super::shared::settings::{SettingsManager, SettingsScope};
use super::shared::{backups, data_health, supabase_config};
use super::AppState;
use crate::db::PaDb;

const NO_DATA_DIR_SUPABASE: &str =
    "no data dir: the daemon was started without --data-dir, so there is no supabase.json to read or write";
const NO_DATA_DIR_BACKUPS: &str =
    "no data dir: the daemon was started without --data-dir, so there is no backups directory";
const NO_DATA_DIR_SETTINGS: &str =
    "no settings store: the daemon was started without --data-dir, so there is no ikenga.db (settings_kv) to open";
const NO_HOME_SETTINGS: &str =
    "no settings store: the daemon has no HOME in its environment, so there is no ~/.ikenga/settings.json to resolve";

// ─── argument helpers ────────────────────────────────────────────────────────

/// The first of `names` present in `args` with a non-null value.
fn arg<'a>(args: &'a Value, names: &[&str]) -> Option<&'a Value> {
    names
        .iter()
        .find_map(|name| args.get(*name).filter(|v| !v.is_null()))
}

/// An optional string argument; present-but-not-a-string is a caller error,
/// as it is when Tauri deserializes an `Option<String>`.
fn opt_str(args: &Value, names: &[&str]) -> Result<Option<String>, String> {
    match arg(args, names) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{}` must be a string", names[0])),
    }
}

fn req_str(args: &Value, names: &[&str]) -> Result<String, String> {
    opt_str(args, names)?.ok_or_else(|| format!("`{}` is required", names[0]))
}

fn opt_bool(args: &Value, names: &[&str]) -> Result<Option<bool>, String> {
    match arg(args, names) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("`{}` must be a boolean", names[0])),
    }
}

/// `Ok(v)` → success with `v`'s JSON; `Err(e)` → `"<cmd>: <e>"`.
fn respond<T: serde::Serialize>(cmd: &str, result: Result<T, String>) -> RpcResponse {
    match result {
        Ok(v) => RpcResponse::success(v),
        Err(e) => RpcResponse::error(format!("{cmd}: {e}")),
    }
}

fn data_dir<'a>(state: &'a AppState, missing: &str) -> Result<&'a Path, String> {
    state
        .config
        .data_dir
        .as_deref()
        .ok_or_else(|| missing.to_string())
}

fn pa_db(state: &AppState) -> Result<&PaDb, String> {
    state
        .pa_db
        .as_deref()
        .ok_or_else(|| super::rpc::NO_DB.to_string())
}

// ─── Supabase config ─────────────────────────────────────────────────────────

pub(super) fn supabase_config_get(state: &AppState) -> RpcResponse {
    let r = data_dir(state, NO_DATA_DIR_SUPABASE).and_then(supabase_config::get);
    respond("supabase_config_get", r)
}

pub(super) fn supabase_config_set(state: &AppState, args: &Value) -> RpcResponse {
    let r = (|| {
        let dir = data_dir(state, NO_DATA_DIR_SUPABASE)?;
        let url = req_str(args, &["url"])?;
        let anon_key = req_str(args, &["anonKey", "anon_key"])?;
        // Absent / null → preserve the stored key; "" → clear it. Same three
        // cases as the desktop, decided in the shared core.
        let service_role_key = opt_str(args, &["serviceRoleKey", "service_role_key"])?;
        supabase_config::set(dir, url, anon_key, service_role_key)
    })();
    respond("supabase_config_set", r)
}

pub(super) fn supabase_config_clear(state: &AppState) -> RpcResponse {
    let r = data_dir(state, NO_DATA_DIR_SUPABASE).and_then(supabase_config::clear);
    respond("supabase_config_clear", r)
}

// ─── Settings ────────────────────────────────────────────────────────────────

/// The daemon's `SettingsManager`: the desktop's, rooted at `--data-dir`
/// (`ikenga.db` for `settings_kv`, `screenshot-config.json`) with no change
/// notifier — so no `settings://changed` emits and no file watchers.
///
/// Initialized on first use, not at boot: `PaDb` is lazy so a daemon nobody
/// queries never touches `ikenga.db`, and `initialize` opens it (and runs the
/// kv → file migration). A failed initialize is logged and retried on the next
/// call; the call itself still goes to the manager, whose own
/// migration-ready gate answers exactly as it does on a desktop whose boot-time
/// initialize failed.
pub(crate) struct DaemonSettings {
    manager: SettingsManager,
    ready: OnceCell<()>,
}

impl DaemonSettings {
    pub(crate) fn new(db: Arc<PaDb>, data_dir: PathBuf, home: PathBuf) -> Self {
        Self {
            manager: SettingsManager::with_notifier(None, db, data_dir, home),
            ready: OnceCell::new(),
        }
    }

    async fn manager(&self) -> &SettingsManager {
        let init = self
            .ready
            .get_or_try_init(|| async { self.manager.initialize().await })
            .await;
        if let Err(e) = init {
            warn!("[settings] initialization failed: {e}");
        }
        &self.manager
    }
}

async fn settings(state: &AppState) -> Result<&SettingsManager, String> {
    match &state.settings {
        Some(s) => Ok(s.manager().await),
        None if state.config.data_dir.is_none() || state.pa_db.is_none() => {
            Err(NO_DATA_DIR_SETTINGS.to_string())
        }
        None => Err(NO_HOME_SETTINGS.to_string()),
    }
}

pub(super) async fn settings_get(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let key = req_str(args, &["key"])?;
        settings(state).await?.get_legacy(&key).await
    }
    .await;
    respond("settings_get", r)
}

pub(super) async fn settings_set(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let key = req_str(args, &["key"])?;
        let value = req_str(args, &["value"])?;
        settings(state).await?.set_legacy(&key, &value).await
    }
    .await;
    respond("settings_set", r)
}

pub(super) async fn settings_get_all(state: &AppState) -> RpcResponse {
    let r = async { settings(state).await?.get_all().await }.await;
    respond("settings_get_all", r)
}

/// Destructive, with the desktop's exact blast radius (`SettingsManager::
/// clear_all`): the settings-owned `settings_kv` rows, the personal
/// `settings.json`, and `<data-dir>/screenshot-config.json`. Nothing else.
pub(super) async fn settings_clear_all(state: &AppState) -> RpcResponse {
    let r = async { settings(state).await?.clear_all().await }.await;
    respond("settings_clear_all", r)
}

pub(super) async fn settings_read_file(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let scope = opt_str(args, &["scope"])?;
        let scope = SettingsScope::parse(scope.as_deref().unwrap_or("project"))?;
        let project_id = opt_str(args, &["projectId", "project_id"])?;
        settings(state)
            .await?
            .read(scope, project_id.as_deref())
            .await
    }
    .await;
    respond("settings_read_file", r)
}

pub(super) async fn settings_write_field(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let scope = SettingsScope::parse(&req_str(args, &["scope"])?)?;
        let field = req_str(args, &["field"])?;
        // Tauri's `value: Value` takes JSON null as a value (a removal sends
        // one), so read it raw rather than through `arg`, which skips nulls.
        let value = args
            .get("value")
            .cloned()
            .ok_or_else(|| "`value` is required".to_string())?;
        let project_id = opt_str(args, &["projectId", "project_id"])?;
        let remove = opt_bool(args, &["remove"])?.unwrap_or(false);
        settings(state)
            .await?
            .write_field(scope, project_id.as_deref(), &field, value, remove)
            .await
    }
    .await;
    respond("settings_write_field", r)
}

// ─── Data health ─────────────────────────────────────────────────────────────

pub(super) async fn data_health_scan(state: &AppState) -> RpcResponse {
    let r = async { data_health::scan(pa_db(state)?).await }.await;
    respond("data_health_scan", r)
}

/// The daemon's `ikenga.db` under `--data-dir`, stat'ed only.
pub(super) fn data_health_db_size(state: &AppState) -> RpcResponse {
    respond(
        "data_health_db_size",
        pa_db(state).and_then(data_health::db_size),
    )
}

// ─── Backups (list / delete) ─────────────────────────────────────────────────

/// `<data-dir>/backups` — the daemon's one data dir standing in for the
/// desktop's `app_local_data_dir`, with the same `backups` leaf.
fn backups_dir(state: &AppState) -> Result<PathBuf, String> {
    Ok(data_dir(state, NO_DATA_DIR_BACKUPS)?.join(backups::BACKUPS_DIR))
}

pub(super) fn backup_list(state: &AppState) -> RpcResponse {
    let r = backups_dir(state).and_then(|dir| backups::list(&dir));
    respond("backup_list", r)
}

pub(super) fn backup_delete(state: &AppState, args: &Value) -> RpcResponse {
    let r = (|| {
        let dir = backups_dir(state)?;
        let path = req_str(args, &["path"])?;
        backups::delete(&dir, &path)
    })();
    respond("backup_delete", r)
}

// ─── Pkg settings ────────────────────────────────────────────────────────────

/// Schema from the daemon's `--pkgs-dir` index, values from `pkg_settings`.
/// An unknown pkg is the desktop's answer: `schema: null` + whatever rows the
/// table holds for that id — not an error.
///
/// `pkg_settings_set` is NOT served (see `desktop_only.toml`):
/// `pkg_settings.pkg_id` REFERENCES `pkg_installed(id)` and sqlx enforces
/// foreign keys, so an upsert needs an installed row — which the daemon, which
/// installs nothing, never has. On a daemon `ikenga.db` the stored rows here
/// are therefore whatever the file already holds; normally none, so `values`
/// is the manifest defaults.
pub(super) async fn pkg_settings_get(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let pkg_id = req_str(args, &["pkgId", "pkg_id"])?;
        let db = pa_db(state)?;
        let schema = state.pkg_index.settings_schema(&pkg_id);
        crate::pkg::settings_values::get(db, pkg_id, schema).await
    }
    .await;
    respond("pkg_settings_get", r)
}

#[cfg(test)]
mod tests {
    //! House pattern (see the slice-1 tests in `server/mod.rs`): a literal
    //! `ServerConfig` → the router → `oneshot` POST `/api/rpc` with the bearer
    //! token. `router_with_home` is `create_router` with the settings home
    //! pinned to a temp dir, so nothing here can touch the real `~/.ikenga`.

    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::Router;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use crate::db::PaDb;
    use crate::engines::EngineRegistry;
    use crate::executor::ExecutorTier;
    use crate::pty::PtyManager;
    use crate::server::shared::settings::{schema, SettingsManager, SettingsScope};
    use crate::server::shared::{backups, data_health, supabase_config};
    use crate::server::{router_with_home, ServerConfig};

    fn config(data_dir: Option<PathBuf>, pkgs_dir: Option<PathBuf>) -> ServerConfig {
        ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            static_dir: PathBuf::from("no-spa-here"),
            pkgs_dir,
            data_dir,
            auth_token: Some("tok".into()),
            allowed_origins: vec![],
            idle_timeout_secs: None,
            executor_tier: ExecutorTier::T0,
        }
    }

    /// A daemon with `--data-dir`, `--pkgs-dir` and a home, all under one
    /// tempdir. `db` is the same `PaDb` the router holds.
    struct Daemon {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        data: PathBuf,
        home: PathBuf,
        pkgs: PathBuf,
        db: Arc<PaDb>,
        router: Router,
    }

    fn daemon() -> Daemon {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (data, home, pkgs) = (root.join("data"), root.join("home"), root.join("pkgs"));
        for d in [&data, &home, &pkgs] {
            std::fs::create_dir_all(d).unwrap();
        }
        write_settings_pkg(&pkgs);
        let db = Arc::new(PaDb::new(data.join("ikenga.db")));
        let router = router_with_home(
            config(Some(data.clone()), Some(pkgs.clone())),
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            Some(db.clone()),
            None,
            Some(home.clone()),
        );
        Daemon {
            _tmp: tmp,
            root,
            data,
            home,
            pkgs,
            db,
            router,
        }
    }

    /// No `--data-dir` (so no `PaDb` either), with or without a home.
    fn bare(home: Option<PathBuf>) -> Router {
        router_with_home(
            config(None, None),
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            None,
            None,
            home,
        )
    }

    async fn rpc(router: &Router, cmd: &str, args: Value) -> Value {
        let res = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/rpc")
                    .header("authorization", "Bearer tok")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "cmd": cmd, "args": args }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The `data` of a successful response (`null` for a unit result).
    async fn ok(router: &Router, cmd: &str, args: Value) -> Value {
        let res = rpc(router, cmd, args.clone()).await;
        assert_eq!(res["ok"], true, "{cmd} {args} → {res}");
        res.get("data").cloned().unwrap_or(Value::Null)
    }

    async fn err(router: &Router, cmd: &str, args: Value) -> String {
        let res = rpc(router, cmd, args.clone()).await;
        assert_eq!(res["ok"], false, "{cmd} {args} should fail → {res}");
        res["error"].as_str().unwrap().to_string()
    }

    // ── Supabase config ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn supabase_config_keeps_the_desktop_semantics() {
        let d = daemon();
        let r = &d.router;
        let file = d.data.join(supabase_config::FILENAME);

        assert_eq!(ok(r, "supabase_config_get", json!({})).await, Value::Null);

        // camelCase (what tauri-cmd.ts sends).
        let set = json!({
            "url": "https://x.supabase.co", "anonKey": "anon1", "serviceRoleKey": "srk1"
        });
        assert_eq!(ok(r, "supabase_config_set", set).await, Value::Null);
        let got = ok(r, "supabase_config_get", json!({})).await;
        assert_eq!(
            got,
            json!({
                "url": "https://x.supabase.co", "anon_key": "anon1", "service_role_key": "srk1"
            })
        );
        // Same serialized type as the desktop command returns.
        assert_eq!(
            got,
            serde_json::to_value(supabase_config::get(&d.data).unwrap()).unwrap()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "supabase.json must be owner-only");
        }

        // snake_case, key absent → preserved.
        let set = json!({ "url": "https://y.supabase.co", "anon_key": "anon2" });
        ok(r, "supabase_config_set", set).await;
        let got = ok(r, "supabase_config_get", json!({})).await;
        assert_eq!(got["url"], "https://y.supabase.co");
        assert_eq!(got["anon_key"], "anon2");
        assert_eq!(
            got["service_role_key"], "srk1",
            "absent key must be preserved"
        );

        // null (what `serviceRoleKey ?? null` sends) → preserved too.
        let set = json!({ "url": "u", "anonKey": "a", "serviceRoleKey": null });
        ok(r, "supabase_config_set", set).await;
        let got = ok(r, "supabase_config_get", json!({})).await;
        assert_eq!(got["service_role_key"], "srk1");

        // explicit "" → cleared (field omitted, as the desktop serializes it).
        let set = json!({ "url": "u", "anonKey": "a", "service_role_key": "" });
        ok(r, "supabase_config_set", set).await;
        let got = ok(r, "supabase_config_get", json!({})).await;
        assert!(got.get("service_role_key").is_none(), "{got}");

        let e = err(
            r,
            "supabase_config_set",
            json!({ "url": " ", "anonKey": "a" }),
        )
        .await;
        assert!(e.contains("url and anon_key are required"), "{e}");
        let e = err(r, "supabase_config_set", json!({ "url": "u" })).await;
        assert!(e.contains("`anonKey` is required"), "{e}");

        assert_eq!(ok(r, "supabase_config_clear", json!({})).await, Value::Null);
        assert!(!file.exists());
        assert_eq!(ok(r, "supabase_config_get", json!({})).await, Value::Null);
    }

    #[tokio::test]
    async fn supabase_config_without_data_dir_names_the_flag() {
        let r = bare(None);
        for (cmd, args) in [
            ("supabase_config_get", json!({})),
            ("supabase_config_set", json!({ "url": "u", "anonKey": "a" })),
            ("supabase_config_clear", json!({})),
        ] {
            let e = err(&r, cmd, args).await;
            assert!(e.contains("--data-dir"), "{cmd}: {e}");
        }
    }

    // ── Secrets ─────────────────────────────────────────────────────────────

    /// The daemon's names are its own namespace's: `secrets_env::list_keys`,
    /// in the desktop's bare `string[]` shape. No data dir needed.
    #[tokio::test]
    async fn secrets_index_names_is_the_env_namespace() {
        for r in [daemon().router, bare(None)] {
            let names = ok(&r, "secrets_index_names", json!({})).await;
            assert!(names.is_array(), "{names}");
            assert_eq!(names, json!(crate::secrets_env::list_keys()));
        }
    }

    // ── Settings ────────────────────────────────────────────────────────────

    /// A second, directly-constructed manager over the same db / data dir /
    /// home: what the desktop command would return for the same state.
    async fn direct_manager(d: &Daemon) -> SettingsManager {
        let m = SettingsManager::with_notifier(None, d.db.clone(), d.data.clone(), d.home.clone());
        m.initialize().await.unwrap();
        m
    }

    #[tokio::test]
    async fn settings_read_write_read_round_trips_in_the_desktop_shape() {
        let d = daemon();
        let r = &d.router;
        let personal = d.home.join(".ikenga").join("settings.json");

        // First use runs `initialize`, whose kv → file migration writes the
        // personal file — on the desktop too, at boot.
        let before = ok(r, "settings_read_file", json!({ "scope": "personal" })).await;
        assert_eq!(before["personalPresent"], true);
        assert_eq!(before["personalPath"], personal.to_string_lossy().as_ref());
        assert_ne!(before["personal"]["appearance"]["theme"], "B");

        let args = json!({
            "scope": "personal", "field": "appearance.theme", "value": "B",
            "remove": false, "projectId": null
        });
        let written = ok(r, "settings_write_field", args).await;
        assert_eq!(written["personal"]["appearance"]["theme"], "B");

        let after = ok(r, "settings_read_file", json!({ "scope": "personal" })).await;
        assert_eq!(after["personalPresent"], true);
        assert_eq!(after["personal"]["appearance"]["theme"], "B");
        assert!(std::fs::read_to_string(&personal)
            .unwrap()
            .contains("\"B\""));

        // Byte-for-byte the core's `SettingsReadResult`.
        let direct = direct_manager(&d)
            .await
            .read(SettingsScope::Personal, None)
            .await
            .unwrap();
        assert_eq!(after, serde_json::to_value(direct).unwrap());

        // Removal: the FE sends the field with `remove: true`.
        let args = json!({
            "scope": "personal", "field": "appearance.theme", "value": null, "remove": true
        });
        let removed = ok(r, "settings_write_field", args).await;
        assert_ne!(removed["personal"]["appearance"]["theme"], "B");

        let args = json!({ "scope": "personal", "field": "appearance.theme" });
        let e = err(r, "settings_write_field", args).await;
        assert!(e.contains("`value` is required"), "{e}");
        let e = err(r, "settings_read_file", json!({ "scope": "nope" })).await;
        assert!(e.starts_with("settings_read_file: "), "{e}");
    }

    #[tokio::test]
    async fn settings_project_scope_takes_both_spellings() {
        let d = daemon();
        let r = &d.router;
        let root = d.root.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        crate::db::exec(
            &d.db,
            "INSERT INTO projects (id, display_name, root_path, position, is_default, created_at) \
             VALUES ('p1', 'P1', ?, 1, 0, 0)",
            &[json!(root.to_string_lossy())],
        )
        .await
        .unwrap();

        let args = json!({
            "scope": "project", "field": "appearance.theme", "value": "C", "project_id": "p1"
        });
        ok(r, "settings_write_field", args).await;
        let camel = json!({ "scope": "project", "projectId": "p1" });
        let snake = json!({ "scope": "project", "project_id": "p1" });
        let camel = ok(r, "settings_read_file", camel).await;
        let snake = ok(r, "settings_read_file", snake).await;
        assert_eq!(camel, snake);
        assert_eq!(camel["projectId"], "p1");
        assert_eq!(camel["projectPresent"], true);
        assert_eq!(camel["effective"]["appearance"]["theme"], "C");
        assert_eq!(
            camel["projectPath"],
            root.join(".ikenga")
                .join("settings.json")
                .to_string_lossy()
                .as_ref()
        );

        let args = json!({ "scope": "project", "projectId": "nope" });
        let e = err(r, "settings_read_file", args).await;
        assert!(e.contains("project not found"), "{e}");
    }

    #[tokio::test]
    async fn settings_kv_get_set_get_all() {
        let d = daemon();
        let r = &d.router;

        // Not a settings-owned key → plain kv row.
        ok(
            r,
            "settings_set",
            json!({ "key": "wp19.probe", "value": "v1" }),
        )
        .await;
        assert_eq!(
            ok(r, "settings_get", json!({ "key": "wp19.probe" })).await,
            "v1"
        );
        let absent = ok(r, "settings_get", json!({ "key": "wp19.absent" })).await;
        assert_eq!(absent, Value::Null);

        // A legacy key → written through to the personal file.
        let args = json!({ "key": "appearance.theme", "value": "\"C\"" });
        ok(r, "settings_set", args).await;
        let theme = ok(r, "settings_get", json!({ "key": "appearance.theme" })).await;
        let direct = direct_manager(&d).await;
        let expected = direct.get_legacy("appearance.theme").await.unwrap();
        assert_eq!(theme, json!(expected));
        let personal = d.home.join(".ikenga").join("settings.json");
        assert!(std::fs::read_to_string(personal).unwrap().contains("\"C\""));

        let all = ok(r, "settings_get_all", json!({})).await;
        assert_eq!(all["wp19.probe"], "v1");
        assert_eq!(all, json!(direct.get_all().await.unwrap()));

        let e = err(r, "settings_set", json!({ "key": "k" })).await;
        assert!(e.contains("`value` is required"), "{e}");
    }

    /// `settings_clear_all` deletes exactly what the desktop deletes: the
    /// settings-owned kv rows (`schema::is_settings_owned_key`), the personal
    /// file and `<data-dir>/screenshot-config.json`. Every other kv row and
    /// every other file survives.
    #[tokio::test]
    async fn settings_clear_all_has_the_desktop_blast_radius() {
        let d = daemon();
        let r = &d.router;

        let owned = json!({ "key": "appearance.theme", "value": "\"B\"" });
        ok(r, "settings_set", owned).await;
        let foreign = json!({ "key": "wp19.foreign", "value": "keep" });
        ok(r, "settings_set", foreign).await;
        let active = json!({ "key": "shell.activeProjectId", "value": "default" });
        ok(r, "settings_set", active).await;
        let personal = d.home.join(".ikenga").join("settings.json");
        let screenshot = d.data.join("screenshot-config.json");
        std::fs::write(&screenshot, br#"{"override_dir":null}"#).unwrap();
        let bystanders = [
            d.home.join(".ikenga").join("actions.json"),
            d.data.join("supabase.json"),
            d.data.join("unrelated.txt"),
        ];
        for f in &bystanders {
            std::fs::write(f, b"{}").unwrap();
        }
        assert!(personal.exists());

        type Kv = std::collections::BTreeMap<String, String>;
        async fn kv(db: &PaDb) -> Kv {
            let q = "SELECT key, value FROM settings_kv ORDER BY key";
            crate::db::query_json(db, q, &[])
                .await
                .unwrap()
                .into_iter()
                .map(|row| {
                    let k = row["key"].as_str().unwrap().to_string();
                    (k, row["value"].as_str().unwrap().to_string())
                })
                .collect()
        }
        let before = kv(&d.db).await;
        assert!(before.keys().any(|k| schema::is_settings_owned_key(k)));

        assert_eq!(ok(r, "settings_clear_all", json!({})).await, Value::Null);

        let after = kv(&d.db).await;
        let expected: Kv = before
            .into_iter()
            .filter(|(k, _)| !schema::is_settings_owned_key(k))
            .collect();
        assert_eq!(after, expected);
        assert_eq!(after["wp19.foreign"], "keep");
        assert_eq!(after["shell.activeProjectId"], "default");

        assert!(
            !personal.exists(),
            "personal settings.json is desktop-cleared"
        );
        assert!(
            !screenshot.exists(),
            "screenshot-config.json is desktop-cleared"
        );
        for f in &bystanders {
            assert!(f.exists(), "{} must survive clear_all", f.display());
        }
        assert!(d.data.join("ikenga.db").exists());
    }

    #[tokio::test]
    async fn settings_name_what_is_missing() {
        let cmds = [
            ("settings_get", json!({ "key": "k" })),
            ("settings_set", json!({ "key": "k", "value": "v" })),
            ("settings_get_all", json!({})),
            ("settings_clear_all", json!({})),
            ("settings_read_file", json!({ "scope": "personal" })),
            (
                "settings_write_field",
                json!({ "scope": "personal", "field": "appearance.theme", "value": "B" }),
            ),
        ];
        let no_data = bare(Some(PathBuf::from("/nonexistent-home")));
        for (cmd, args) in &cmds {
            let e = err(&no_data, cmd, args.clone()).await;
            assert!(e.contains("--data-dir"), "{cmd}: {e}");
        }

        // Data dir + db, but no home: nowhere to put the personal file.
        let tmp = tempfile::tempdir().unwrap();
        let no_home = router_with_home(
            config(Some(tmp.path().to_path_buf()), None),
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            Some(Arc::new(PaDb::new(tmp.path().join("ikenga.db")))),
            None,
            None,
        );
        for (cmd, args) in &cmds {
            let e = err(&no_home, cmd, args.clone()).await;
            assert!(e.contains("HOME"), "{cmd}: {e}");
        }
    }

    // ── Data health ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn data_health_matches_the_shared_core() {
        let d = daemon();
        let r = &d.router;

        assert_eq!(ok(r, "data_health_scan", json!({})).await, json!([]));

        crate::db::exec(
            &d.db,
            "INSERT INTO sales_deals (id, company, research_item_id) \
             VALUES ('deal-orphan', 'Beta', 'rn-missing')",
            &[],
        )
        .await
        .unwrap();
        let scan = ok(r, "data_health_scan", json!({})).await;
        let pool = d.db.ensure_reader_pool().await.unwrap();
        let direct = data_health::scan_orphans(&pool).await.unwrap();
        assert_eq!(scan, serde_json::to_value(direct).unwrap());
        assert_eq!(scan[0]["table"], "sales_deals");
        assert_eq!(scan[0]["sample_ids"], json!(["deal-orphan"]));

        let size = ok(r, "data_health_db_size", json!({})).await;
        let db_path = d.data.join("ikenga.db");
        assert_eq!(size["db_path"], db_path.to_string_lossy().as_ref());
        assert!(size["db_bytes"].as_u64().unwrap() > 0);
        let direct = data_health::measure_db_files(&db_path).unwrap();
        assert_eq!(size, serde_json::to_value(direct).unwrap());

        let r = bare(None);
        for cmd in ["data_health_scan", "data_health_db_size"] {
            let e = err(&r, cmd, json!({})).await;
            assert!(e.contains("--data-dir"), "{cmd}: {e}");
        }
    }

    // ── Backups ─────────────────────────────────────────────────────────────

    fn write_ikbak(path: &Path, created_at: &str) {
        use std::io::Write;
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file::<_, ()>("manifest.json", zip::write::FileOptions::default())
            .unwrap();
        let manifest = json!({
            "format_version": 3, "schema_version": 7, "created_at": created_at,
            "hostname": "h", "username": "u", "path_mode": "tokenized",
            "home_dir": "/home/u", "has_secrets": true, "pkg_count": 2
        });
        zip.write_all(manifest.to_string().as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    #[tokio::test]
    async fn backup_list_and_delete_stay_inside_the_backups_dir() {
        let d = daemon();
        let r = &d.router;
        let dir = d.data.join(backups::BACKUPS_DIR);

        // No dir yet → empty, as on desktop.
        assert_eq!(ok(r, "backup_list", json!({})).await, json!([]));

        std::fs::create_dir_all(&dir).unwrap();
        write_ikbak(&dir.join("old.ikbak"), "2026-01-01T00:00:00Z");
        write_ikbak(&dir.join("new.ikbak"), "2026-09-01T00:00:00Z");
        std::fs::write(dir.join("broken.ikbak"), b"not a zip").unwrap();
        std::fs::write(dir.join("notes.txt"), b"ignored").unwrap();

        let list = ok(r, "backup_list", json!({})).await;
        let direct = backups::list(&dir).unwrap();
        assert_eq!(list, serde_json::to_value(direct).unwrap());
        let rows = list.as_array().unwrap();
        assert_eq!(rows.len(), 3, "{list}");
        assert_eq!(
            rows[0]["created_at"], "2026-09-01T00:00:00Z",
            "newest first"
        );
        assert_eq!(rows[0]["path_mode"], "tokenized");
        assert_eq!(rows[0]["pkg_count"], 2);
        assert_eq!(rows[2]["created_at"], "", "unreadable bundle still listed");

        // Outside the dir: refused, and the target survives.
        let victim = d.data.join("ikenga.db");
        std::fs::write(&victim, b"x").unwrap();
        let escape = dir.join("..").join("ikenga.db");
        for path in [escape.to_string_lossy().into_owned(), "/etc/passwd".into()] {
            let e = err(r, "backup_delete", json!({ "path": path })).await;
            assert!(
                e.contains("refusing to delete file outside backups dir"),
                "{path}: {e}"
            );
        }
        assert!(victim.exists());
        // A symlink inside the dir pointing out is resolved before the check.
        #[cfg(unix)]
        {
            let link = dir.join("link.ikbak");
            std::os::unix::fs::symlink(&victim, &link).unwrap();
            let e = err(r, "backup_delete", json!({ "path": link })).await;
            assert!(e.contains("outside backups dir"), "{e}");
            assert!(victim.exists());
            std::fs::remove_file(&link).unwrap();
        }

        let target = dir.join("old.ikbak");
        let res = ok(r, "backup_delete", json!({ "path": target })).await;
        assert_eq!(res, Value::Null);
        assert!(!target.exists());
        let list = ok(r, "backup_list", json!({})).await;
        assert_eq!(list.as_array().unwrap().len(), 2);

        let r = bare(None);
        let e = err(&r, "backup_list", json!({})).await;
        assert!(e.contains("--data-dir"), "{e}");
        let e = err(&r, "backup_delete", json!({ "path": "/x" })).await;
        assert!(e.contains("--data-dir"), "{e}");
    }

    // ── Pkg settings ────────────────────────────────────────────────────────

    fn write_settings_pkg(pkgs: &Path) {
        let dir = pkgs.join("com.test.settings");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            r#"{"id":"com.test.settings","name":"S","version":"0.1.0","ikenga_api":"1",
                "settings":{"schema":[
                  {"key":"theme","type":"string","label":"Theme","default":"dark"},
                  {"key":"count","type":"number","label":"Count","default":3}
                ]}}"#,
        )
        .unwrap();
        let bare = pkgs.join("com.test.nosettings");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::write(
            bare.join("manifest.json"),
            r#"{"id":"com.test.nosettings","name":"N","version":"0.1.0","ikenga_api":"1"}"#,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn pkg_settings_get_merges_manifest_defaults_with_stored_rows() {
        use crate::pkg::settings_values;

        let d = daemon();
        let r = &d.router;
        let id = "com.test.settings";
        let pkg = crate::pkg::manifest::Package::load(&d.pkgs.join(id)).unwrap();
        let schema = settings_values::declared_schema(&pkg);

        let got = ok(r, "pkg_settings_get", json!({ "pkgId": id })).await;
        assert_eq!(got["values"], json!({ "theme": "dark", "count": 3 }));
        assert_eq!(got["schema"][0]["key"], "theme");
        let direct = settings_values::get(&d.db, id.into(), schema.clone())
            .await
            .unwrap();
        assert_eq!(got, serde_json::to_value(direct).unwrap());

        // Rows as a desktop-written `ikenga.db` holds them (the FK needs the
        // `pkg_installed` row first; the daemon never writes either).
        crate::db::exec(
            &d.db,
            "INSERT INTO pkg_installed \
             (id, version, ikenga_api, manifest_json, install_path, enabled, installed_at) \
             VALUES (?, '0.1.0', '1', '{}', '/x', 1, 0)",
            &[json!(id)],
        )
        .await
        .unwrap();
        for (key, value) in [("theme", json!("light")), ("extra", json!({ "a": [1] }))] {
            settings_values::set(&d.db, id, key, &value).await.unwrap();
        }
        let snake = ok(r, "pkg_settings_get", json!({ "pkg_id": id })).await;
        assert_eq!(
            snake["values"],
            json!({ "theme": "light", "count": 3, "extra": { "a": [1] } })
        );
        let direct = settings_values::get(&d.db, id.into(), schema)
            .await
            .unwrap();
        assert_eq!(snake, serde_json::to_value(direct).unwrap());

        // No settings block, and unknown: schema null, not an error — the
        // desktop's `SettingsRegistry::schema_for` is `None` for both.
        for other in ["com.test.nosettings", "com.test.unknown"] {
            let got = ok(r, "pkg_settings_get", json!({ "pkgId": other })).await;
            assert_eq!(
                got,
                json!({ "pkg_id": other, "schema": null, "values": {} })
            );
        }

        let e = err(r, "pkg_settings_get", json!({})).await;
        assert!(e.contains("`pkgId` is required"), "{e}");

        let r = bare(None);
        let e = err(&r, "pkg_settings_get", json!({ "pkgId": id })).await;
        assert!(e.contains("--data-dir"), "{e}");
    }
}
