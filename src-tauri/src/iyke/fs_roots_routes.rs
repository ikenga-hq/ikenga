//! The fs allowlist on the iyke bridge: `GET /iyke/fs-roots` lists the
//! folders the shell may open; `POST /iyke/fs-roots/add` asks for one more.
//!
//! An add is never granted silently. A folder that is already covered
//! answers at once; anything else becomes an Allow / Deny prompt in the
//! main window (`iyke://fs-roots-add-request`, answered through
//! `iyke_fs_roots_request_done`, the `rpc.rs` request/oneshot pattern). The
//! folder is added only on Allow.
//!
//! Why a prompt and not a plain add: the bridge token already reaches
//! `terminal/spawn`, so the allowlist is not what stops a token holder on
//! the desktop. What it does give is visibility: the user sees which folder
//! an agent wants, and why, before the shell will open it. There is no
//! bridge remove: narrowing the list stays a Settings action.
//!
//! Desktop only. The network server (`ikenga-server`) has its own list and
//! its own `fs_roots_*` arms, where the allowlist is the boundary.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use axum::{extract::Json as JsonBody, http::StatusCode, Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::AppHandle;

use super::rpc::{self, new_pending, Pending};
use crate::fs_roots::FsRoots;

/// How long the shell waits for the user to answer. A person has to read
/// the prompt and click, so this is long next to the other FE round trips.
pub const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);

/// Longest `reason` shown in the prompt; longer ones are cut.
const MAX_REASON_CHARS: usize = 300;

/// Pending prompts. The FE reply is `{ "allow": bool }`.
pub fn fs_roots_pending() -> &'static Pending<Value> {
    static PENDING: std::sync::OnceLock<Pending<Value>> = std::sync::OnceLock::new();
    PENDING.get_or_init(new_pending)
}

/// FE → Rust: the user answered one `iyke://fs-roots-add-request` prompt.
#[tauri::command]
pub async fn iyke_fs_roots_request_done(request_id: String, allow: bool) -> Result<(), String> {
    rpc::resolve(fs_roots_pending(), &request_id, json!({ "allow": allow }))
        .await
        .map_err(|e| e.to_string())
}

/// What the prompt shows. `path` is what the caller sent; `resolved` is the
/// folder it names (expanded, canonical), so `~` or `..` can't hide where it
/// points.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AddPrompt {
    pub path: String,
    pub resolved: String,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddBody {
    /// The folder: absolute, or starting with `~`.
    pub path: String,
    /// Shown to the user: why the caller wants it ("open the akuko artifacts").
    #[serde(default)]
    pub reason: Option<String>,
}

type Refusal = (StatusCode, Json<Value>);

fn refuse(status: StatusCode, error: &str, message: impl Into<String>) -> Refusal {
    (
        status,
        Json(json!({ "error": error, "message": message.into() })),
    )
}

/// Check a requested folder before anyone is asked: absolute after `~` /
/// env expansion, existing, a directory. Returns the canonical folder.
pub fn resolve_requested_folder(input: &str) -> Result<PathBuf, Refusal> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(refuse(StatusCode::BAD_REQUEST, "bad_path", "path is empty"));
    }
    let expanded = shellexpand::full(trimmed)
        .map(|c| PathBuf::from(c.into_owned()))
        .map_err(|e| {
            refuse(
                StatusCode::BAD_REQUEST,
                "bad_path",
                format!("expand {trimmed}: {e}"),
            )
        })?;
    if !expanded.is_absolute() {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "bad_path",
            format!("give an absolute folder path (or one starting with ~), not {trimmed}"),
        ));
    }
    let canonical = expanded.canonicalize().map_err(|e| {
        refuse(
            StatusCode::BAD_REQUEST,
            "bad_path",
            format!("{trimmed} does not exist or can't be read ({e})"),
        )
    })?;
    if !canonical.is_dir() {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "bad_path",
            format!("{trimmed} is not a folder"),
        ));
    }
    Ok(canonical)
}

fn clean_reason(reason: Option<String>) -> Option<String> {
    let r = reason?.trim().to_string();
    if r.is_empty() {
        return None;
    }
    Some(if r.chars().count() > MAX_REASON_CHARS {
        let mut cut: String = r.chars().take(MAX_REASON_CHARS).collect();
        cut.push('…');
        cut
    } else {
        r
    })
}

/// The add, minus the transport: `ask` puts the prompt to the user and
/// resolves to their answer (`Err` = no answer). Split out so tests drive
/// allow / deny / no-answer against a local [`FsRoots`] without a window.
pub async fn add_with_prompt<F, Fut>(
    roots: &FsRoots,
    body: AddBody,
    ask: F,
) -> Result<Json<Value>, Refusal>
where
    F: FnOnce(AddPrompt) -> Fut,
    Fut: Future<Output = anyhow::Result<bool>>,
{
    let folder = resolve_requested_folder(&body.path)?;
    if roots.is_allowed(&folder) {
        return Ok(Json(json!({
            "schema_version": 1,
            "added": false,
            "already_allowed": true,
            "roots": roots.list_inputs(),
        })));
    }
    let prompt = AddPrompt {
        path: body.path.trim().to_string(),
        resolved: folder.to_string_lossy().into_owned(),
        reason: clean_reason(body.reason),
    };
    match ask(prompt).await {
        Ok(true) => {}
        Ok(false) => {
            return Err(refuse(
                StatusCode::FORBIDDEN,
                "denied",
                "the user declined to let Ikenga open this folder",
            ))
        }
        Err(e) => {
            return Err(refuse(
                StatusCode::REQUEST_TIMEOUT,
                "no_answer",
                format!("no answer from the user: {e}"),
            ))
        }
    }
    // Store the canonical folder, not the caller's spelling: it is what the
    // user saw and approved.
    let roots_now = roots.add(&folder.to_string_lossy()).map_err(|e| {
        refuse(
            StatusCode::INTERNAL_SERVER_ERROR,
            "add_failed",
            e.to_string(),
        )
    })?;
    Ok(Json(json!({
        "schema_version": 1,
        "added": true,
        "already_allowed": false,
        "roots": roots_now,
    })))
}

fn installed_roots() -> Result<std::sync::Arc<FsRoots>, Refusal> {
    crate::fs_roots::current().ok_or_else(|| {
        refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "not_ready",
            "the folder list is not loaded yet; retry once the shell has started",
        )
    })
}

// --- GET /iyke/fs-roots -------------------------------------------------------

/// `GET /iyke/fs-roots` — the folders the shell may open, as the user wrote
/// them (`~` kept).
pub async fn get_fs_roots() -> Result<Json<Value>, Refusal> {
    let roots = installed_roots()?;
    Ok(Json(json!({
        "schema_version": 1,
        "roots": roots.list_inputs(),
    })))
}

// --- POST /iyke/fs-roots/add --------------------------------------------------

/// `POST /iyke/fs-roots/add {path, reason?}` — ask the user to let the shell
/// open one more folder. 200 `{added, already_allowed, roots}`; 400 for a
/// path that isn't an existing absolute folder; 403 when the user declines;
/// 408 when they don't answer within [`PROMPT_TIMEOUT`].
pub async fn post_fs_roots_add(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<AddBody>,
) -> Result<Json<Value>, Refusal> {
    let roots = installed_roots()?;
    add_with_prompt(&roots, body, |prompt| async move {
        let reply = rpc::request_to(
            &app,
            fs_roots_pending(),
            "main",
            "iyke://fs-roots-add-request",
            PROMPT_TIMEOUT,
            |request_id| {
                json!({
                    "request_id": request_id,
                    "path": prompt.path,
                    "resolved": prompt.resolved,
                    "reason": prompt.reason,
                })
            },
        )
        .await?;
        Ok(reply.get("allow").and_then(Value::as_bool).unwrap_or(false))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn setup() -> (tempfile::TempDir, FsRoots, PathBuf, PathBuf) {
        let tmp = tempfile::TempDir::new().unwrap();
        let allowed = tmp.path().join("allowed");
        let wanted = tmp.path().join("wanted");
        std::fs::create_dir_all(allowed.join("sub")).unwrap();
        std::fs::create_dir_all(&wanted).unwrap();
        let roots = FsRoots::load(tmp.path().join("fs_roots.json")).unwrap();
        roots.add(&allowed.to_string_lossy()).unwrap();
        (tmp, roots, allowed, wanted)
    }

    fn body(path: &std::path::Path, reason: Option<&str>) -> AddBody {
        AddBody {
            path: path.to_string_lossy().into_owned(),
            reason: reason.map(str::to_string),
        }
    }

    fn status(r: &Result<Json<Value>, Refusal>) -> StatusCode {
        match r {
            Ok(_) => StatusCode::OK,
            Err((s, _)) => *s,
        }
    }

    #[tokio::test]
    async fn an_allowed_folder_adds_its_canonical_path() {
        let (_tmp, roots, _allowed, wanted) = setup();
        let seen: Arc<Mutex<Option<AddPrompt>>> = Arc::default();
        let seen_in = seen.clone();
        let r = add_with_prompt(&roots, body(&wanted, Some("  open the artifacts ")), |p| {
            *seen_in.lock().unwrap() = Some(p);
            async { Ok(true) }
        })
        .await;
        let Json(v) = r.unwrap();
        assert_eq!(v["added"], true);
        let canon = wanted.canonicalize().unwrap();
        assert!(roots.is_allowed(&canon.join("a.html")));
        let prompt = seen.lock().unwrap().clone().unwrap();
        assert_eq!(prompt.resolved, canon.to_string_lossy());
        assert_eq!(prompt.reason.as_deref(), Some("open the artifacts"));
    }

    #[tokio::test]
    async fn a_covered_folder_answers_without_asking() {
        let (_tmp, roots, allowed, _wanted) = setup();
        let before = roots.list_inputs();
        let r = add_with_prompt(&roots, body(&allowed.join("sub"), None), |_| async {
            panic!("must not prompt for a folder that is already allowed")
        })
        .await;
        let Json(v) = r.unwrap();
        assert_eq!(v["added"], false);
        assert_eq!(v["already_allowed"], true);
        assert_eq!(roots.list_inputs(), before);
    }

    #[tokio::test]
    async fn a_declined_or_unanswered_prompt_adds_nothing() {
        let (_tmp, roots, _allowed, wanted) = setup();
        let before = roots.list_inputs();
        let denied = add_with_prompt(&roots, body(&wanted, None), |_| async { Ok(false) }).await;
        assert_eq!(status(&denied), StatusCode::FORBIDDEN);
        let silent = add_with_prompt(&roots, body(&wanted, None), |_| async {
            Err(anyhow::anyhow!("timed out"))
        })
        .await;
        assert_eq!(status(&silent), StatusCode::REQUEST_TIMEOUT);
        assert_eq!(roots.list_inputs(), before);
        assert!(!roots.is_allowed(&wanted.canonicalize().unwrap()));
    }

    #[tokio::test]
    async fn bad_paths_are_refused_before_anyone_is_asked() {
        let (tmp, roots, _allowed, _wanted) = setup();
        let file = tmp.path().join("note.txt");
        std::fs::write(&file, "x").unwrap();
        for bad in [
            "".to_string(),
            "relative/dir".to_string(),
            tmp.path().join("missing").to_string_lossy().into_owned(),
            file.to_string_lossy().into_owned(),
        ] {
            let r = add_with_prompt(
                &roots,
                AddBody {
                    path: bad.clone(),
                    reason: None,
                },
                |_| async { panic!("must not prompt for {bad:?}") },
            )
            .await;
            assert_eq!(status(&r), StatusCode::BAD_REQUEST, "{bad:?}");
        }
    }

    #[test]
    fn a_long_reason_is_cut_and_a_blank_one_dropped() {
        assert_eq!(clean_reason(Some("   ".into())), None);
        let long = "x".repeat(MAX_REASON_CHARS + 50);
        let cut = clean_reason(Some(long)).unwrap();
        assert_eq!(cut.chars().count(), MAX_REASON_CHARS + 1);
        assert!(cut.ends_with('…'));
    }
}
