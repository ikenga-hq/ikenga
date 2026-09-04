//! Localhost HTTP transport for the pkg-backend database sandbox (WP-23 / D-18).
//!
//! Thin IO wrapper — every security decision lives in [`crate::pkg::db_scope`].
//! This module only: pulls the per-pkg bearer token off the request, resolves it
//! to a pkg identity, re-reads that pkg's manifest, hands sql + declared scope to
//! `db_scope::authorize`, and (only if cleared) runs the statement through the
//! same `crate::db` entry points the Tauri commands use.
//!
//! # Why HTTP and not a Tauri command
//!
//! A sidecar / MCP server is a separate OS process. It cannot `invoke()`. The
//! iyke server is already the shell's process-to-process seam, so the accessor
//! rides on it.
//!
//! # Auth
//!
//! These two routes are mounted **outside** `require_token`, with their own
//! credential check — the same pattern `/iyke/browser/_reply` uses. That is
//! deliberate: the global iyke bearer token grants terminals, secrets and the
//! whole control surface. Handing that to a third-party pkg backend so it could
//! read five of its own tables would be a far larger widening than the hole this
//! work package closes. Each pkg gets its own token instead, minted at spawn,
//! good for these two endpoints and nothing else.
//!
//! # Wire
//!
//! ```text
//! POST /iyke/pkg-db/query   { sql, params? } -> { ok:true, rows:[…] }
//! POST /iyke/pkg-db/exec    { sql, params? } -> { ok:true, rowsAffected, lastInsertId }
//! refusal (401/403)                          -> { ok:false, reason, error }
//! ```
//!
//! `reason` is a frozen [`crate::pkg::db_scope::DbScopeRefusal`] wire string.

use std::sync::Arc;

use axum::{
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::commands::db::PaDb;
use crate::pkg::db_scope::{self, DbMode, DbScopeDenial, DbScopeRefusal};
use crate::pkg::manifest::Package;

#[derive(Debug, Deserialize)]
pub struct PkgDbReq {
    pub sql: String,
    #[serde(default)]
    pub params: Vec<Value>,
}

fn deny(status: StatusCode, d: &DbScopeDenial) -> axum::response::Response {
    (
        status,
        Json(json!({
            "ok": false,
            "reason": d.reason.as_str(),
            "error": d.detail,
        })),
    )
        .into_response()
}

/// Bearer token off `Authorization`, falling back to `X-Ikenga-Pkg-Token` for
/// clients that would rather not shape an `Authorization` header.
fn presented_token(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        return Some(v.to_string());
    }
    headers
        .get("x-ikenga-pkg-token")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// Resolve the caller to `(pkg_id, declared sqlite.tables)`, or a refusal.
///
/// Fails closed at every step: no token, an unknown token, a manifest that
/// won't load, or a pkg that never declared `capabilities.sqlite` all end here
/// rather than reaching a pool.
fn authenticate(headers: &HeaderMap) -> Result<(String, Vec<String>), (StatusCode, DbScopeDenial)> {
    let token = presented_token(headers).ok_or((
        StatusCode::UNAUTHORIZED,
        DbScopeDenial {
            reason: DbScopeRefusal::UnknownToken,
            detail: "missing pkg db token".to_string(),
        },
    ))?;

    let grant = db_scope::resolve_grant(&token).ok_or((
        StatusCode::UNAUTHORIZED,
        DbScopeDenial {
            reason: DbScopeRefusal::UnknownToken,
            detail: "token does not identify an installed pkg".to_string(),
        },
    ))?;

    // Re-read the manifest per request so a scope narrowed on disk (or a dev
    // re-mount) takes effect without a shell restart.
    let pkg = Package::load(&grant.install_path).map_err(|e| {
        log::warn!(
            "[pkg-db] manifest load failed for `{}` at {}: {e:#}",
            grant.pkg_id,
            grant.install_path.display()
        );
        (
            StatusCode::FORBIDDEN,
            DbScopeDenial {
                reason: DbScopeRefusal::ManifestUnreadable,
                detail: "pkg manifest could not be read".to_string(),
            },
        )
    })?;

    let scope = db_scope::declared_scope(&pkg.manifest).ok_or((
        StatusCode::FORBIDDEN,
        DbScopeDenial {
            reason: DbScopeRefusal::CapabilityMissing,
            detail: "pkg lacks the 'sqlite' capability".to_string(),
        },
    ))?;

    Ok((grant.pkg_id, scope))
}

async fn handle(
    mode: DbMode,
    headers: HeaderMap,
    db: Arc<PaDb>,
    req: PkgDbReq,
) -> axum::response::Response {
    let (pkg_id, scope) = match authenticate(&headers) {
        Ok(v) => v,
        Err((status, d)) => return deny(status, &d),
    };

    let tables = match db_scope::authorize(mode, &req.sql, &scope) {
        Ok(t) => t,
        Err(d) => {
            // Refusals here are the interesting ones — a backend reaching
            // outside its manifest. Log with the pkg id so an operator can see
            // which pkg tried what.
            log::warn!(
                "[pkg-db] REFUSED pkg=`{pkg_id}` reason={} detail={}",
                d.reason.as_str(),
                d.detail
            );
            return deny(StatusCode::FORBIDDEN, &d);
        }
    };
    log::debug!(
        "[pkg-db] pkg=`{pkg_id}` {} tables={tables:?}",
        match mode {
            DbMode::Read => "query",
            DbMode::Write => "exec",
        }
    );

    match mode {
        DbMode::Read => match crate::db::query_json(&db, &req.sql, &req.params).await {
            Ok(rows) => Json(json!({ "ok": true, "rows": rows })).into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "reason": "query-failed", "error": e })),
            )
                .into_response(),
        },
        DbMode::Write => match crate::db::exec(&db, &req.sql, &req.params).await {
            Ok(r) => Json(json!({
                "ok": true,
                "rowsAffected": r.rows_affected,
                "lastInsertId": r.last_insert_id,
            }))
            .into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "reason": "exec-failed", "error": e })),
            )
                .into_response(),
        },
    }
}

pub async fn post_pkg_db_query(
    Extension(db): Extension<Arc<PaDb>>,
    headers: HeaderMap,
    Json(req): Json<PkgDbReq>,
) -> axum::response::Response {
    handle(DbMode::Read, headers, db, req).await
}

pub async fn post_pkg_db_exec(
    Extension(db): Extension<Arc<PaDb>>,
    headers: HeaderMap,
    Json(req): Json<PkgDbReq>,
) -> axum::response::Response {
    handle(DbMode::Write, headers, db, req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn reads_the_token_from_either_header() {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_static("Bearer abc123"));
        assert_eq!(presented_token(&h).as_deref(), Some("abc123"));

        let mut h = HeaderMap::new();
        h.insert("x-ikenga-pkg-token", HeaderValue::from_static("def456"));
        assert_eq!(presented_token(&h).as_deref(), Some("def456"));

        assert_eq!(presented_token(&HeaderMap::new()), None);
    }

    #[test]
    fn a_bare_authorization_value_is_not_accepted() {
        // Must be `Bearer <tok>`; a naked token in Authorization is ignored.
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_static("abc123"));
        assert_eq!(presented_token(&h), None);
    }

    #[test]
    fn no_token_fails_closed_before_any_db_work() {
        let err = authenticate(&HeaderMap::new()).unwrap_err();
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
        assert_eq!(err.1.reason, DbScopeRefusal::UnknownToken);
    }

    // ---------------------------------------------------------------------
    // End-to-end through the real handler, a real manifest on disk, and a real
    // (temp) ikenga.db. This is the WP-23 DoD: a pkg backend that tries a table
    // outside its manifest scope is *refused*, demonstrated rather than asserted
    // to be likely.
    // ---------------------------------------------------------------------

    /// Write a manifest scoped to the meetings tables and mint a grant for it,
    /// exactly as a spawn site would. Returns (tempdir, auth headers).
    ///
    /// `pkg_id` is a per-test parameter on purpose: grants are process-global
    /// and keyed by pkg id (one id = one install path, which is true of a real
    /// install), so two concurrently-running tests sharing an id would fight
    /// over whose temp dir the grant points at.
    fn scoped_pkg(pkg_id: &str) -> (tempfile::TempDir, HeaderMap) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("manifest.json"),
            format!(
                r#"{{
              "id": "{pkg_id}",
              "name": "Test Meetings",
              "version": "0.0.1",
              "ikenga_api": "3",
              "permissions": {{ "sqlite.tables": ["meetings", "meeting_speakers"] }},
              "capabilities": {{ "sqlite": {{ "db": "ikenga.local" }} }}
            }}"#
            ),
        )
        .expect("write manifest");

        let token = db_scope::issue_grant(pkg_id, dir.path());
        let mut h = HeaderMap::new();
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        (dir, h)
    }

    async fn body_json(r: axum::response::Response) -> (StatusCode, Value) {
        let status = r.status();
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 20)
            .await
            .expect("read body");
        (status, serde_json::from_slice(&bytes).expect("json body"))
    }

    #[tokio::test]
    async fn backend_can_reach_its_own_tables() {
        let (_dir, headers) = scoped_pkg("com.ikenga.test.meetings.happy");
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = Arc::new(PaDb::new(tmp.path().join("ikenga.db")));

        let (status, body) = body_json(
            handle(
                DbMode::Write,
                headers.clone(),
                db.clone(),
                PkgDbReq {
                    sql: "INSERT INTO meetings (id, title, platform, status, start_time, created_at, updated_at) \
                          VALUES ('m1', 'Standup', 'local', 'done', '2026-09-04T09:00:00Z', '2026-09-04T09:00:00Z', '2026-09-04T09:00:00Z')"
                        .to_string(),
                    params: vec![],
                },
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "in-scope insert: {body}");
        assert_eq!(body["ok"], json!(true));

        let (status, body) = body_json(
            handle(
                DbMode::Read,
                headers,
                db,
                PkgDbReq {
                    sql: "SELECT id, title FROM meetings".to_string(),
                    params: vec![],
                },
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "in-scope select: {body}");
        assert_eq!(body["rows"][0]["title"], json!("Standup"));
    }

    /// THE NEGATIVE CASE. A backend scoped to the meetings tables asks for
    /// `tasks` — the user's real task list — on both the read and the write
    /// path. Both are refused, and the pre-seeded task row is still there
    /// afterwards, proving the refusal happened before the statement ran.
    #[tokio::test]
    async fn backend_is_refused_a_table_outside_its_manifest_scope() {
        let (_dir, headers) = scoped_pkg("com.ikenga.test.meetings.refusal");
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = Arc::new(PaDb::new(tmp.path().join("ikenga.db")));

        // Seed a task the way the rest of the shell would — NOT through the
        // sandbox, so the sandbox's refusal is the only thing under test.
        crate::db::exec(
            &db,
            "INSERT INTO tasks (id, title, status) VALUES ('t1', 'Do not touch me', 'pending')",
            &[],
        )
        .await
        .expect("seed task");

        // 1. Reading someone else's table.
        let (status, body) = body_json(
            handle(
                DbMode::Read,
                headers.clone(),
                db.clone(),
                PkgDbReq {
                    sql: "SELECT * FROM tasks".to_string(),
                    params: vec![],
                },
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["ok"], json!(false));
        assert_eq!(body["reason"], json!("table-out-of-scope"));
        assert!(body["error"].as_str().unwrap().contains("tasks"), "{body}");

        // 2. Joining it onto a table it *is* allowed — no smuggling.
        let (status, body) = body_json(
            handle(
                DbMode::Read,
                headers.clone(),
                db.clone(),
                PkgDbReq {
                    sql: "SELECT m.id, t.title FROM meetings m JOIN tasks t ON t.id = m.id"
                        .to_string(),
                    params: vec![],
                },
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["reason"], json!("table-out-of-scope"));

        // 3. Deleting from it.
        let (status, body) = body_json(
            handle(
                DbMode::Write,
                headers.clone(),
                db.clone(),
                PkgDbReq {
                    sql: "DELETE FROM tasks".to_string(),
                    params: vec![],
                },
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["reason"], json!("table-out-of-scope"));

        // 4. Hiding a second statement behind an allowed first one.
        let (status, body) = body_json(
            handle(
                DbMode::Write,
                headers.clone(),
                db.clone(),
                PkgDbReq {
                    sql: "UPDATE meetings SET title = 'x'; DELETE FROM tasks".to_string(),
                    params: vec![],
                },
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["reason"], json!("multiple-statements"));

        // 5. Dropping the table outright.
        let (status, body) = body_json(
            handle(
                DbMode::Write,
                headers.clone(),
                db.clone(),
                PkgDbReq {
                    sql: "DROP TABLE tasks".to_string(),
                    params: vec![],
                },
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["reason"], json!("statement-not-allowed"));

        // The task is untouched — every refusal happened before the pool.
        let rows = crate::db::query_json(&db, "SELECT title FROM tasks", &[])
            .await
            .expect("tasks still queryable");
        assert_eq!(rows.len(), 1, "the seeded task must survive");
        assert_eq!(rows[0]["title"], json!("Do not touch me"));
    }

    /// A pkg whose manifest never declared `capabilities.sqlite` gets nothing,
    /// even for a table it happens to name in `permissions`.
    #[tokio::test]
    async fn backend_without_the_sqlite_capability_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("manifest.json"),
            r#"{
              "id": "com.ikenga.test.nocap",
              "name": "No Cap",
              "version": "0.0.1",
              "ikenga_api": "3",
              "permissions": { "sqlite.tables": ["meetings"] }
            }"#,
        )
        .expect("write manifest");
        let token = db_scope::issue_grant("com.ikenga.test.nocap", dir.path());
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );

        let tmp = tempfile::tempdir().expect("tempdir");
        let db = Arc::new(PaDb::new(tmp.path().join("ikenga.db")));
        let (status, body) = body_json(
            handle(
                DbMode::Read,
                headers,
                db,
                PkgDbReq {
                    sql: "SELECT * FROM meetings".to_string(),
                    params: vec![],
                },
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["reason"], json!("capability-missing"));
    }

    /// A grant pointing at a directory with no manifest fails closed rather
    /// than defaulting to an empty-but-permissive scope.
    #[tokio::test]
    async fn a_grant_with_no_manifest_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let token = db_scope::issue_grant("com.ikenga.test.nomanifest", dir.path());
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        let err = authenticate(&headers).unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        assert_eq!(err.1.reason, DbScopeRefusal::ManifestUnreadable);
    }

    #[test]
    fn an_unknown_token_fails_closed() {
        let mut h = HeaderMap::new();
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer 00000000000000000000000000000000"),
        );
        let err = authenticate(&h).unwrap_err();
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
        assert_eq!(err.1.reason, DbScopeRefusal::UnknownToken);
    }
}
