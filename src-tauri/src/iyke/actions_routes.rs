//! WP-62: the `iyke` surface for actions, menus and keys — the bridge half
//! of D-06's tab-footer lines (`iyke actions list|set|import`, `iyke menus
//! show`, `iyke keys list|set`). Contract: G-ACTIONS
//! (`plans/shell-ux-rearchitecture/drafts/actions-schema.md`) and
//! G-ACTIONS-API (`src/lib/actions/store.ts` header).
//!
//! Two halves, on purpose:
//!
//! - **Reads** (`GET /iyke/actions`, `GET /iyke/menus/:id`) serve a
//!   FE-pushed mirror of the effective model, the same convention
//!   `FrameMirror` already uses for `GET /iyke/keys`: the frontend computes
//!   `getEffectiveModel()` (WP-52) and pushes a projection through
//!   `iyke_set_actions_frame` whenever `subscribeEffectiveModel()` fires
//!   (`use-iyke-shell-sync.ts`). The wire shape is opaque to Rust here —
//!   same convention as `ShellSnapshot.panes` (`state.rs`) — the frontend
//!   owns the schema (`IykeActionMirror` / `IykeMenuMirror`,
//!   `src/lib/tauri-cmd.ts`). 503 until the first push, same as `/iyke/keys`.
//! - **Writes** (`POST /iyke/actions/set`, `/iyke/actions/import`,
//!   `/iyke/keys/set`) and the live `GET /iyke/keys/resolve` query round-trip
//!   into the frontend through the exact request/oneshot pattern `rpc.rs`
//!   already uses for the DOM / query-cache / wait handlers: the frontend's
//!   `use-iyke-shell-sync.ts` listener calls the real G-ACTIONS-API write
//!   functions (`saveUserAction`, `addKeybinding`) and the live
//!   `resolveKeypress()` query — the exact functions the D-06 UI itself
//!   calls. The CLI and the UI therefore share one code path into the one
//!   WP-50 Rust validator, never two implementations of it. This is also
//!   why the DEC-55 / DEC-65 gating ("an OS-scope keys set is
//!   personal-only", "a project-scope keys set is written held until
//!   trusted") needs no extra code here: it's enforced by the same
//!   validator and the same merge the UI goes through.

use std::time::Duration;

use axum::{
    extract::{Json as JsonBody, Path, Query},
    http::StatusCode,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::Value;
use tauri::AppHandle;

use super::rpc::{self, new_pending, Pending};

/// FE round trips here are in-app function calls (no network, no child
/// process) — generous relative to the DOM/query-cache RPCs but still
/// bounded so a wedged frontend fails a CLI call instead of hanging it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// FE → Rust mirror of the effective model's actions + menus. Held
/// separately from `handlers::FrameMirror` so this file owns its whole
/// surface (mirror, pending map, commands, handlers) without editing that
/// struct's call sites.
#[derive(Debug, Default)]
pub struct ActionsFrameMirror {
    actions: tokio::sync::RwLock<Option<Value>>,
    menus: tokio::sync::RwLock<Option<Value>>,
}

impl ActionsFrameMirror {
    /// Partial-update semantics like `FrameMirror::set` (WP-21): a `None`
    /// field leaves the stored value untouched.
    pub async fn set(&self, actions: Option<Value>, menus: Option<Value>) {
        if actions.is_some() {
            *self.actions.write().await = actions;
        }
        if menus.is_some() {
            *self.menus.write().await = menus;
        }
    }

    pub async fn actions(&self) -> Option<Value> {
        self.actions.read().await.clone()
    }

    pub async fn menus(&self) -> Option<Value> {
        self.menus.read().await.clone()
    }
}

/// Process-wide mirror, mirroring `handlers::frame_mirror()`'s pattern.
pub fn actions_frame_mirror() -> &'static ActionsFrameMirror {
    static MIRROR: std::sync::OnceLock<ActionsFrameMirror> = std::sync::OnceLock::new();
    MIRROR.get_or_init(ActionsFrameMirror::default)
}

/// Shared pending map for the four write/query round trips below. Every
/// request mints its own UUID (`rpc::request`), so one map safely serves
/// all four call sites — not worth a dedicated `IykeRpc` field each for a
/// handful of callers, unlike the high-traffic DOM/query-cache RPCs.
pub fn actions_pending() -> &'static Pending<Value> {
    static PENDING: std::sync::OnceLock<Pending<Value>> = std::sync::OnceLock::new();
    PENDING.get_or_init(new_pending)
}

/// FE → Rust push: `use-iyke-shell-sync.ts` calls this whenever
/// `subscribeEffectiveModel()` fires, so `GET /iyke/actions` and
/// `GET /iyke/menus/:id` never serve a stale merge after a file edit,
/// a package install, or a project switch.
#[tauri::command]
pub async fn iyke_set_actions_frame(actions: Option<Value>, menus: Option<Value>) -> Result<(), String> {
    actions_frame_mirror().set(actions, menus).await;
    Ok(())
}

/// FE → Rust callback resolving one of the four round trips below
/// (`rpc::resolve`). `result` is opaque JSON handed straight back as the
/// HTTP response body (or turned into an error response — see
/// `write_result_to_response`).
#[tauri::command]
pub async fn iyke_actions_request_done(request_id: String, result: Value) -> Result<(), String> {
    rpc::resolve(actions_pending(), &request_id, result)
        .await
        .map_err(|e| e.to_string())
}

fn unavailable(what: &str) -> (StatusCode, String) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        format!(
            "{what} not pushed yet: the shell frontend publishes its effective model at \
             workspace mount and on every actions/keybindings change \
             (use-iyke-shell-sync); retry once the workspace has mounted"
        ),
    )
}

/// A round trip's FE reply is `{ ok: bool, error?: string, ...rest }`
/// (`use-iyke-shell-sync.ts`). `ok: true` passes the whole payload back as
/// the response body; `ok: false` becomes a 422 with the FE's message —
/// the same `ActionsValidationError` text the D-06 UI would show.
fn write_result_to_response(result: Value) -> Result<Json<Value>, (StatusCode, String)> {
    let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if ok {
        Ok(Json(result))
    } else {
        let message = result
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("write refused by the actions/keybindings validator")
            .to_string();
        Err((StatusCode::UNPROCESSABLE_ENTITY, message))
    }
}

// --- GET /iyke/actions -------------------------------------------------------

#[derive(Deserialize)]
pub struct ActionsListQuery {
    /// `personal` | `project` | `all` (default). Filters the mirrored
    /// actions by their `source` field — matches D-06's `actions list
    /// --scope <scope>` footer line, where scope names the file being
    /// viewed (G-ACTIONS §1.2), not a merge-layer selector.
    #[serde(default)]
    pub scope: Option<String>,
}

/// `GET /iyke/actions` body, split out so tests can drive a fresh mirror
/// (the process-wide `actions_frame_mirror()` can't be un-set between
/// tests — same reason `handlers::keys_response` is split the same way).
async fn actions_list_response(
    mirror: &ActionsFrameMirror,
    scope: Option<&str>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let Some(actions) = mirror.actions().await else {
        return Err(unavailable("actions"));
    };
    let all = actions.as_array().cloned().unwrap_or_default();
    let filtered = match scope {
        Some(s) if s != "all" => all
            .into_iter()
            .filter(|a| a.get("source").and_then(Value::as_str) == Some(s))
            .collect::<Vec<_>>(),
        _ => all,
    };
    Ok(Json(serde_json::json!({
        "schema_version": 1,
        "count": filtered.len(),
        "actions": filtered,
    })))
}

/// `GET /iyke/actions` — the last effective-actions mirror the FE pushed,
/// optionally filtered by `?scope=`. 503 until the first push.
pub async fn get_actions_list(
    Query(q): Query<ActionsListQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    actions_list_response(actions_frame_mirror(), q.scope.as_deref()).await
}

// --- GET /iyke/menus/:menu_id -------------------------------------------------

/// `GET /iyke/menus/:menu_id` body, split out for the same test-isolation
/// reason as `actions_list_response`.
async fn menu_response(
    mirror: &ActionsFrameMirror,
    menu_id: &str,
) -> Result<Json<Value>, (StatusCode, String)> {
    let Some(menus) = mirror.menus().await else {
        return Err(unavailable("menus"));
    };
    match menus.get(menu_id) {
        Some(menu) => Ok(Json(serde_json::json!({
            "schema_version": 1,
            "menu": menu,
        }))),
        None => Err((
            StatusCode::NOT_FOUND,
            format!(
                "no menu {menu_id:?} in the effective model — G-ACTIONS §1.3 lists the frozen \
                 menu ids (files, artifacts, session, palette, native/<top>, section/<id>, …)"
            ),
        )),
    }
}

/// `GET /iyke/menus/:menu_id` — one effective menu (G-ACTIONS §1.3) from the
/// last mirror the FE pushed. 503 before the first push; 404 for a menu id
/// the current model doesn't know (unknown ids are kept-and-warned in the
/// files, never rendered — G-ACTIONS §1.6 `W_UNKNOWN_MENU`).
pub async fn get_menu(Path(menu_id): Path<String>) -> Result<Json<Value>, (StatusCode, String)> {
    menu_response(actions_frame_mirror(), &menu_id).await
}

// --- POST /iyke/actions/set ---------------------------------------------------

#[derive(Deserialize)]
pub struct ActionsSetBody {
    /// `personal` | `project` (G-ACTIONS §1.1).
    pub scope: String,
    /// A `UserAction` (G-ACTIONS §1.2): `{id, name, icon?, description?,
    /// run, placements?, scope}`. Validated FE-side by the same WP-50
    /// validator `saveUserAction` uses — an invalid document never reaches
    /// disk (`E_*` codes, G-ACTIONS §1.6).
    pub action: Value,
}

/// `POST /iyke/actions/set` — upserts one user action by id (`saveUserAction`,
/// G-ACTIONS-API). Round-trips into the frontend; see the module doc for why.
pub async fn post_actions_set(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<ActionsSetBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let result = rpc::request(
        &app,
        actions_pending(),
        "iyke://actions-set-request",
        REQUEST_TIMEOUT,
        move |request_id| {
            serde_json::json!({
                "request_id": request_id,
                "scope": body.scope,
                "action": body.action,
            })
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    write_result_to_response(result)
}

// --- POST /iyke/actions/import -------------------------------------------------

#[derive(Deserialize)]
pub struct ActionsImportBody {
    pub scope: String,
    /// A batch of `UserAction` documents, upserted one at a time
    /// (`saveUserAction` per item) so one invalid action doesn't sink the
    /// rest — same "never override an existing binding without saying so"
    /// spirit as WP-61's import, scaled down to what a CLI batch needs.
    pub actions: Vec<Value>,
}

/// `POST /iyke/actions/import` — upserts a batch of user actions.
pub async fn post_actions_import(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<ActionsImportBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let result = rpc::request(
        &app,
        actions_pending(),
        "iyke://actions-import-request",
        REQUEST_TIMEOUT,
        move |request_id| {
            serde_json::json!({
                "request_id": request_id,
                "scope": body.scope,
                "actions": body.actions,
            })
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    // Import always returns 200 with a per-item report (added/skipped/
    // errors) rather than refusing whole — a single malformed entry
    // shouldn't sink an otherwise-valid batch.
    Ok(Json(result))
}

// --- POST /iyke/keys/set ------------------------------------------------------

#[derive(Deserialize)]
pub struct KeysSetBody {
    /// `personal` | `project`. A project write is `written held until
    /// trusted` (DEC-65) — that's the merge's job (WP-52), not this
    /// route's; the write itself succeeds like any other project edit.
    pub scope: String,
    /// Registry key grammar (G-ACTIONS §3): `mod+shift+e`, or a two-stroke
    /// chord `mod+k mod+r`.
    pub key: String,
    /// An action id; a leading `-` makes a negative rule (§1.5).
    pub command: String,
    #[serde(default)]
    pub when: Option<String>,
    /// `app` (default) or `os`. `os` is refused outside the personal file
    /// (`E_OS_LAYER`) by the same validator the UI's Keys tab uses — this
    /// route adds no separate check.
    #[serde(default)]
    pub key_scope: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
}

/// `POST /iyke/keys/set` — adds one keybinding rule (`addKeybinding`,
/// G-ACTIONS-API). Not a full rebind (it never negates a prior rule on the
/// same key): scripted parity with D-06's "Rebind…" flow for the common
/// case of binding an unbound or additionally-bound command. See the
/// WP-62 report for this as a recorded drift.
pub async fn post_keys_set(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<KeysSetBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let result = rpc::request(
        &app,
        actions_pending(),
        "iyke://keys-set-request",
        REQUEST_TIMEOUT,
        move |request_id| {
            serde_json::json!({
                "request_id": request_id,
                "scope": body.scope,
                "key": body.key,
                "command": body.command,
                "when": body.when,
                "key_scope": body.key_scope,
                "platform": body.platform,
            })
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    write_result_to_response(result)
}

// --- GET /iyke/keys/resolve ----------------------------------------------------

#[derive(Deserialize)]
pub struct KeysResolveQuery {
    /// A key sequence in registry grammar (`mod+k`), matched in the
    /// platform-resolved form (G-ACTIONS §2.3).
    pub key: String,
    #[serde(default)]
    pub platform: Option<String>,
}

/// `GET /iyke/keys/resolve` — the WP-62 hand-off's "what fires here": the
/// live `resolveKeypress()` winner for `key` against the current
/// (default: live) context, round-tripped into the frontend since context
/// keys (focus, active pane/project) only exist there.
pub async fn get_keys_resolve(
    Extension(app): Extension<AppHandle>,
    Query(q): Query<KeysResolveQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let result = rpc::request(
        &app,
        actions_pending(),
        "iyke://keys-resolve-request",
        REQUEST_TIMEOUT,
        move |request_id| {
            serde_json::json!({
                "request_id": request_id,
                "key": q.key,
                "platform": q.platform,
            })
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn actions_mirror_is_503_until_pushed() {
        let mirror = ActionsFrameMirror::default();
        assert!(mirror.actions().await.is_none());
    }

    #[tokio::test]
    async fn actions_mirror_partial_update_keeps_menus() {
        let mirror = ActionsFrameMirror::default();
        mirror
            .set(Some(json!([{"id": "a"}])), Some(json!({"files": {"id": "files"}})))
            .await;
        // A push with actions only (menus: None) must not clobber menus —
        // same partial-update contract as `handlers::FrameMirror`.
        mirror.set(Some(json!([{"id": "b"}])), None).await;
        assert_eq!(mirror.actions().await.unwrap(), json!([{"id": "b"}]));
        assert_eq!(
            mirror.menus().await.unwrap(),
            json!({"files": {"id": "files"}})
        );
    }

    #[test]
    fn write_result_maps_ok_and_error() {
        let ok = write_result_to_response(json!({"ok": true, "written": true}));
        assert!(ok.is_ok());

        let err = write_result_to_response(json!({"ok": false, "error": "E_ID_GRAMMAR: bad id"}));
        let (status, message) = err.unwrap_err();
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(message.contains("E_ID_GRAMMAR"));
    }

    #[test]
    fn write_result_defaults_message_when_error_field_absent() {
        let err = write_result_to_response(json!({"ok": false}));
        let (status, message) = err.unwrap_err();
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(message.contains("refused"));
    }

    #[tokio::test]
    async fn get_actions_list_filters_by_scope() {
        let mirror = ActionsFrameMirror::default();
        mirror
            .set(
                Some(json!([
                    {"id": "a", "source": "personal"},
                    {"id": "b", "source": "project"},
                    {"id": "c", "source": "builtin"}
                ])),
                None,
            )
            .await;
        let Json(personal_only) = actions_list_response(&mirror, Some("personal")).await.unwrap();
        assert_eq!(personal_only["count"], 1);
        let Json(all) = actions_list_response(&mirror, None).await.unwrap();
        assert_eq!(all["count"], 3);
    }

    #[tokio::test]
    async fn get_actions_list_503s_until_pushed() {
        let mirror = ActionsFrameMirror::default();
        let err = actions_list_response(&mirror, None).await.unwrap_err();
        assert_eq!(err.0, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn get_menu_404s_for_unknown_id() {
        let mirror = ActionsFrameMirror::default();
        mirror
            .set(None, Some(json!({"files": {"id": "files", "items": []}})))
            .await;
        let err = menu_response(&mirror, "nonexistent-menu-id").await.unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_menu_returns_known_menu() {
        let mirror = ActionsFrameMirror::default();
        mirror
            .set(None, Some(json!({"files": {"id": "files", "items": []}})))
            .await;
        let Json(body) = menu_response(&mirror, "files").await.unwrap();
        assert_eq!(body["menu"]["id"], "files");
    }
}
