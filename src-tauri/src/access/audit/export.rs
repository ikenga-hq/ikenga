//! `access_audit_export` and `ikenga-server audit export` (G-ACCESS §6.8),
//! WP-77.
//!
//! JSONL: one object per row with **every** column — `detail` as the
//! stored string and the hashes as lowercase hex, so a contiguous range
//! verifies standalone from its first row's `prev_hash` — then one manifest
//! line:
//!
//! ```json
//! {"type":"manifest","schema":"ikenga-audit-export-v1","store_id":"…","first_seq":1,
//!  "last_seq":512,"head_hash":"…","contiguous":true,"verified":true,"broken_at_seq":null,
//!  "filter":{…},"exported_at_ms":…}
//! ```
//!
//! (plus `rows`, `truncated` and `resealed`: the breaks an operator
//! acknowledged). The chain is verified before every export
//! (§6.4) and every export appends `audit.exported {rows, filter}`.
//!
//! **`destPath` (normative, review C-08, A-34).** Honoured only for the T0
//! operator bearer — the desktop, whose daemon runs as the same user. Every
//! other credential, and every T1 request (the broker serves `access_*` as
//! root), gets `invalid_request` and no file is written: the broker never
//! writes a file on a principal's behalf. Without `destPath` the reply is
//! `{jsonl, truncated}`, capped at 50 000 rows (the newest).

use std::path::Path;

use serde_json::{json, Value};

use super::chain::{db_head, now_ms, StoredRow};
use super::list::{may_read, select_rows, store, Filter, Visibility};
use super::{record, AuditVia, Event};
use crate::access::ctx::AccessCtx;
use crate::access::rpc::Env;
use crate::access::store::{AccessStore, StoreTier};
use crate::access::{AccessError, Code};

/// The browser download cap (§6.8).
pub const BROWSER_CAP: i64 = 50_000;
pub const SCHEMA: &str = "ikenga-audit-export-v1";

/// One built export.
#[derive(Debug, Clone)]
pub struct Built {
    pub jsonl: String,
    pub rows: usize,
    pub truncated: bool,
    pub manifest: Value,
}

/// One row as an export line (§6.8: every column, hashes hex).
pub fn row_line(r: &StoredRow) -> Value {
    json!({
        "seq": r.seq,
        "at_ms": r.at_ms,
        "kind": r.kind,
        "category": r.category,
        "principal_id": r.principal_id,
        "device_id": r.device_id,
        "via": r.via,
        "subject_principal_id": r.subject_principal_id,
        "subject_device_id": r.subject_device_id,
        "project_key": r.project_key,
        "target": r.target,
        "remote_addr": r.remote_addr,
        "user_agent": r.user_agent,
        "detail": r.detail,
        "prev_hash": hex::encode(&r.prev_hash),
        "hash": hex::encode(&r.hash),
    })
}

/// Verify (§6.4), select, and render. `cap` keeps the newest `cap` rows.
pub async fn build(
    store: &AccessStore,
    filter: &Filter,
    vis: &Visibility,
    cap: Option<i64>,
) -> anyhow::Result<Built> {
    let mut conn = store.pool().acquire().await?;
    let report = store.chain().verify_boot(&mut conn).await?;
    let mut rows = select_rows(&mut conn, filter, vis, None, true, cap.map(|c| c + 1)).await?;
    let truncated = cap.is_some_and(|c| rows.len() as i64 > c);
    if let Some(c) = cap {
        rows.truncate(c as usize);
    }
    rows.reverse();
    let head = db_head(&mut conn).await?;
    let mut jsonl = String::new();
    for r in &rows {
        jsonl.push_str(&row_line(r).to_string());
        jsonl.push('\n');
    }
    let first = rows.first().map(|r| r.seq);
    let last = rows.last().map(|r| r.seq);
    let contiguous = match (first, last) {
        (Some(f), Some(l)) => l - f + 1 == rows.len() as i64,
        _ => true,
    };
    let manifest = json!({
        "type": "manifest",
        "schema": SCHEMA,
        "store_id": store.meta().store_id,
        "first_seq": first,
        "last_seq": last,
        "head_hash": head.map(|h| hex::encode(h.hash)),
        "contiguous": contiguous,
        "verified": report.ok(),
        "broken_at_seq": report.broken.as_ref().map(|b| b.broken_at_seq),
        // Breaks an operator resealed (review B-1): `verified: true` means
        // the chain verifies with these acknowledged, not that the rows
        // verify standalone across them.
        "resealed": report.resealed,
        "filter": filter.to_json(),
        "exported_at_ms": now_ms(),
        "rows": rows.len(),
        "truncated": truncated,
    });
    jsonl.push_str(&manifest.to_string());
    jsonl.push('\n');
    Ok(Built {
        jsonl,
        rows: rows.len(),
        truncated,
        manifest,
    })
}

/// `audit.exported {rows, filter}` — best-effort, after the export.
async fn record_exported(store: &AccessStore, ev: Event, built: &Built, filter: &Filter) {
    let ev = ev.detail(json!({
        "rows": built.rows,
        "filter": filter.to_json(),
        "truncated": built.truncated,
    }));
    if let Err(e) = record(store, &ev).await {
        tracing::warn!("audit.exported not recorded: {e}");
    }
}

/// Write `data` to `path`, owner-only, without following links (review
/// M-1): the bytes go to a fresh `0600` file created `O_EXCL|O_NOFOLLOW`
/// next to `path`, which is then renamed over it. An existing `path` must
/// be a regular file the caller owns — a symlink, a directory or someone
/// else's file is refused, and a link is replaced (never written through).
async fn write_private(path: &Path, data: Vec<u8>) -> std::io::Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || write_private_blocking(&path, &data))
        .await
        .map_err(std::io::Error::other)?
}

fn write_private_blocking(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind, Write};
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if !meta.file_type().is_file() {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "refusing to write over something that is not a regular file (a link?)",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                // SAFETY: geteuid has no preconditions and cannot fail.
                if meta.uid() != unsafe { libc::geteuid() } {
                    return Err(Error::new(
                        ErrorKind::PermissionDenied,
                        "refusing to replace a file another user owns",
                    ));
                }
            }
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "the path names no file"))?;
    let tmp = dir.join(format!(
        ".{}.{}-{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        now_ms()
    ));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let res = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// `access_audit_export {filter, destPath?}` (§9.1).
pub async fn dispatch(env: &Env<'_>, ctx: &AccessCtx, args: &Value) -> Result<Value, AccessError> {
    let vis = may_read(env, ctx)?;
    let filter = Filter::parse(args.get("filter"))?;
    let dest = match args.get("destPath") {
        None | Some(Value::Null) => None,
        Some(v) => {
            // A-34: only the T0 operator bearer (the desktop) names a path.
            if env.tier != StoreTier::T0 || !ctx.is_operator() {
                return Err(AccessError::new(
                    Code::InvalidRequest,
                    "`destPath` is accepted only from the desktop; download the export instead",
                ));
            }
            let p = v
                .as_str()
                .map(Path::new)
                .filter(|p| p.is_absolute())
                .ok_or_else(|| {
                    AccessError::new(Code::InvalidRequest, "`destPath` must be an absolute path")
                })?;
            Some(p.to_path_buf())
        }
    };
    let store = store(env)?;
    let cap = if dest.is_some() {
        None
    } else {
        Some(BROWSER_CAP)
    };
    let built = build(store, &filter, &vis, cap)
        .await
        .map_err(AccessError::internal)?;
    let out = match &dest {
        Some(path) => {
            write_private(path, built.jsonl.clone().into_bytes())
                .await
                .map_err(|e| {
                    AccessError::new(
                        Code::InvalidRequest,
                        format!("could not write {}: {e}", path.display()),
                    )
                })?;
            // The rows the file holds (review m-7), not what a client shows.
            json!({ "path": path.display().to_string(), "rows": built.rows })
        }
        None => json!({ "jsonl": built.jsonl, "truncated": built.truncated }),
    };
    record_exported(store, Event::by("audit.exported", ctx), &built, &filter).await;
    Ok(out)
}

/// `ikenga-server audit export`: operator-wide (the root CLI, §6.7), no
/// cap; writes `out` (owner-only) or returns the JSONL for stdout.
pub async fn export_cli(
    store: &AccessStore,
    filter: &Filter,
    out: Option<&Path>,
) -> anyhow::Result<Built> {
    let built = build(store, filter, &Visibility::All, None).await?;
    if let Some(path) = out {
        write_private(path, built.jsonl.clone().into_bytes()).await?;
    }
    record_exported(
        store,
        Event::new("audit.exported", AuditVia::Cli),
        &built,
        filter,
    )
    .await;
    Ok(built)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::audit::chain::{genesis, StoredRow};
    use crate::access::audit::list::tests::env;
    use crate::access::devices::tests::{operator_ctx, pair};
    use crate::access::rpc::dispatch as rpc;
    use crate::access::sockets::Registry;
    use crate::access::Tier;

    fn lines(jsonl: &str) -> Vec<Value> {
        jsonl
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// The export verifies standalone: recompute every hash from the lines.
    #[tokio::test]
    async fn an_unfiltered_export_verifies_standalone() {
        let store = AccessStore::memory_t0().await;
        let reg = Registry::new();
        let e = env(&store, &reg);
        let op = operator_ctx(&store);
        pair(&store, Tier::Dispatch).await;
        let v = rpc(&e, &op, "access_audit_export", &json!({}))
            .await
            .unwrap();
        assert_eq!(v["truncated"], false);
        let ls = lines(v["jsonl"].as_str().unwrap());
        let (manifest, rows) = ls.split_last().unwrap();
        assert_eq!(manifest["type"], "manifest");
        assert_eq!(manifest["schema"], SCHEMA);
        assert_eq!(manifest["contiguous"], true);
        assert_eq!(manifest["verified"], true);
        assert_eq!(manifest["first_seq"], 1);
        let mut prev = genesis(&store.meta().store_id);
        for l in rows {
            let s = |k: &str| l[k].as_str().map(str::to_string);
            let row = StoredRow {
                seq: l["seq"].as_i64().unwrap(),
                at_ms: l["at_ms"].as_i64().unwrap(),
                kind: s("kind").unwrap(),
                category: s("category").unwrap(),
                principal_id: s("principal_id"),
                device_id: s("device_id"),
                via: s("via").unwrap(),
                subject_principal_id: s("subject_principal_id"),
                subject_device_id: s("subject_device_id"),
                project_key: s("project_key"),
                target: s("target"),
                remote_addr: s("remote_addr"),
                user_agent: s("user_agent"),
                detail: s("detail").unwrap(),
                prev_hash: hex::decode(s("prev_hash").unwrap()).unwrap(),
                hash: Vec::new(),
            };
            assert_eq!(hex::encode(prev), s("prev_hash").unwrap());
            prev = row.compute_hash(&prev);
            assert_eq!(hex::encode(prev), s("hash").unwrap());
        }
        assert_eq!(manifest["last_seq"], rows.last().unwrap()["seq"]);
        // The export itself is recorded.
        let last = rpc(&e, &op, "access_audit_list", &json!({"limit": 1}))
            .await
            .unwrap();
        assert_eq!(last["rows"][0]["kind"], "audit.exported");
    }

    #[tokio::test]
    async fn a_filtered_export_is_not_contiguous() {
        let store = AccessStore::memory_t0().await;
        let reg = Registry::new();
        let e = env(&store, &reg);
        let op = operator_ctx(&store);
        for kind in ["app.locked", "vault.locked", "app.unlocked"] {
            rpc(
                &e,
                &op,
                "access_audit_record_local",
                &json!({"kind": kind, "target": "x"}),
            )
            .await
            .unwrap();
        }
        let v = rpc(
            &e,
            &op,
            "access_audit_export",
            &json!({"filter": {"q": "app."}}),
        )
        .await
        .unwrap();
        let ls = lines(v["jsonl"].as_str().unwrap());
        assert_eq!(ls.len(), 3);
        assert_eq!(ls[2]["contiguous"], false);
        assert_eq!(ls[2]["filter"]["q"], "app.");
    }

    /// A-34 (T0 half): only the operator bearer may pass `destPath`, and
    /// then the file is written owner-only.
    #[tokio::test]
    async fn dest_path_is_the_desktops_alone() {
        let store = AccessStore::memory_t0().await;
        let reg = Registry::new();
        let e = env(&store, &reg);
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("ikenga-audit-2026-10-02.jsonl");
        let (row, _) = pair(&store, Tier::Full).await;
        let phone =
            crate::access::audit::list::tests::device_ctx(&store, &row.device_id, Tier::Full);
        let err = rpc(
            &e,
            &phone,
            "access_audit_export",
            &json!({"destPath": dest.display().to_string()}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, Code::InvalidRequest);
        assert!(!dest.exists());
        let op = operator_ctx(&store);
        let v = rpc(
            &e,
            &op,
            "access_audit_export",
            &json!({"destPath": dest.display().to_string()}),
        )
        .await
        .unwrap();
        assert_eq!(v["path"], dest.display().to_string());
        let body = std::fs::read_to_string(&dest).unwrap();
        assert!(body.lines().last().unwrap().contains("\"manifest\""));
        // Review m-7: the reply counts the rows the file holds.
        assert_eq!(v["rows"], body.lines().count() - 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    /// A-34 (T1 half): under T1 `destPath` is refused for every credential
    /// and no file is written.
    #[tokio::test]
    async fn t1_never_writes_a_file() {
        let store = AccessStore::memory_t0().await;
        let reg = Registry::new();
        let mut e = env(&store, &reg);
        e.tier = StoreTier::T1;
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("x.jsonl");
        let op = operator_ctx(&store);
        let err = rpc(
            &e,
            &op,
            "access_audit_export",
            &json!({"destPath": dest.display().to_string()}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, Code::InvalidRequest);
        assert!(!dest.exists());
    }

    /// Review M-1: the export never writes through a link, never replaces
    /// a non-regular file, and is created owner-only.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_export_file_is_written_without_following_links() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let victim = tmp.path().join("victim.txt");
        std::fs::write(&victim, "precious").unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
        let link = tmp.path().join("out.jsonl");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        let err = write_private(&link, b"rows".to_vec()).await.unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "{err}");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
        let mode = std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        assert!(write_private(tmp.path(), b"rows".to_vec()).await.is_err());

        // A new file, and an existing one of ours: replaced, 0600.
        let out = tmp.path().join("new.jsonl");
        write_private(&out, b"one".to_vec()).await.unwrap();
        write_private(&out, b"two".to_vec()).await.unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "two");
        let mode = std::fs::metadata(&out).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // No temp files left behind.
        let names: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
    }
}
