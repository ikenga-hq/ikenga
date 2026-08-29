//! Trust-review modal — capability-diff snapshot store.
//!
//! Stores one row per pkg in `pkg_capability_snapshots` (migration 0021)
//! containing the normalized JSON of the last-approved `capabilities` +
//! `permissions` blocks. The kernel boot path diffs the on-disk manifest
//! against this snapshot; a mismatch parks the pkg out of the registry
//! replay until the user approves (writes a new snapshot) or rejects
//! (uninstalls the pkg).
//!
//! Distinct from `pkg/trust.rs` (Phase 9 sensitive-perms gating at MCP
//! call-time). Both can fire for the same pkg, separately — this one
//! gates the kernel's `register` replay at boot, that one gates
//! per-tool-call MCP `tools/call`.

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::json;
use sqlx::SqlitePool;

use crate::pkg::manifest::Manifest;

/// One stored snapshot row.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub pkg_id: String,
    pub manifest_capabilities_json: String,
    pub approved_at: i64,
    pub approved_by_implicit: bool,
}

/// Stable, sorted JSON form of the manifest's `capabilities` +
/// `permissions` blocks. Vector ordering is normalized so reorder-only
/// changes don't trigger a re-prompt. The result is a `String` (not a
/// `Value`) so equality checks are cheap and the on-disk format is
/// stable.
pub fn normalize(manifest: &Manifest) -> String {
    let perms = &manifest.permissions;
    let mut shell_execute = perms.shell_execute.clone();
    let mut fs_read = perms.fs_read.clone();
    let mut fs_write = perms.fs_write.clone();
    let mut net = perms.net.clone();
    let mut sqlite_tables = perms.sqlite_tables.clone();
    let mut supabase_tables = perms.supabase_tables.clone();
    let mut vault_keys = perms.vault_keys.clone();
    shell_execute.sort();
    fs_read.sort();
    fs_write.sort();
    net.sort();
    sqlite_tables.sort();
    supabase_tables.sort();
    vault_keys.sort();

    let permissions = json!({
        "shell.execute": shell_execute,
        "fs.read": fs_read,
        "fs.write": fs_write,
        "net": net,
        "sqlite.tables": sqlite_tables,
        "supabase.tables": supabase_tables,
        "vault.keys": vault_keys,
    });

    // capabilities block — present as parsed Option<CapabilitiesBlock>.
    // Round-trip through serde_json so the keys land in the same order as
    // the struct definition; nulls are dropped naturally.
    let capabilities = match &manifest.capabilities {
        Some(c) => serde_json::to_value(c).unwrap_or(json!({})),
        None => json!(null),
    };

    serde_json::to_string(&json!({
        "capabilities": capabilities,
        "permissions": permissions,
    }))
    .expect("normalized capability snapshot json")
}

/// True when the two normalized snapshots differ. Strict string equality
/// because `normalize` produces a canonical form.
pub fn capabilities_changed(old: &str, new: &str) -> bool {
    old != new
}

/// Read the latest snapshot for a pkg, if any.
pub async fn fetch(pool: &SqlitePool, pkg_id: &str) -> Result<Option<Snapshot>> {
    let row: Option<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT pkg_id, manifest_capabilities_json, approved_at, approved_by_implicit
           FROM pkg_capability_snapshots
          WHERE pkg_id = ?",
    )
    .bind(pkg_id)
    .fetch_optional(pool)
    .await
    .context("read pkg_capability_snapshots")?;
    Ok(row.map(|(pkg_id, json, approved_at, implicit)| Snapshot {
        pkg_id,
        manifest_capabilities_json: json,
        approved_at,
        approved_by_implicit: implicit != 0,
    }))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Write a snapshot as implicit approval (first install — the install
/// itself is consent, no user prompt fires).
pub async fn write_implicit(pool: &SqlitePool, pkg_id: &str, snapshot_json: &str) -> Result<()> {
    sqlx::query(
        "INSERT OR REPLACE INTO pkg_capability_snapshots
            (pkg_id, manifest_capabilities_json, approved_at, approved_by_implicit)
         VALUES (?, ?, ?, 1)",
    )
    .bind(pkg_id)
    .bind(snapshot_json)
    .bind(now_ms())
    .execute(pool)
    .await
    .context("insert pkg_capability_snapshots (implicit)")?;
    Ok(())
}

/// Write a snapshot as explicit user approval (modal Approve click).
pub async fn write_explicit(pool: &SqlitePool, pkg_id: &str, snapshot_json: &str) -> Result<()> {
    sqlx::query(
        "INSERT OR REPLACE INTO pkg_capability_snapshots
            (pkg_id, manifest_capabilities_json, approved_at, approved_by_implicit)
         VALUES (?, ?, ?, 0)",
    )
    .bind(pkg_id)
    .bind(snapshot_json)
    .bind(now_ms())
    .execute(pool)
    .await
    .context("insert pkg_capability_snapshots (explicit)")?;
    Ok(())
}

/// Delete any snapshot row for a pkg (e.g. on uninstall / dev unregister).
pub async fn delete(pool: &SqlitePool, pkg_id: &str) -> Result<()> {
    sqlx::query("DELETE FROM pkg_capability_snapshots WHERE pkg_id = ?")
        .bind(pkg_id)
        .execute(pool)
        .await
        .context("delete pkg_capability_snapshots")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::manifest::{Manifest, Permissions};

    fn minimal_manifest() -> Manifest {
        Manifest {
            id: "com.test.x".into(),
            name: "T".into(),
            version: "0.1.0".into(),
            ikenga_api: "1".into(),
            kind: None,
            auth_bridge: None,
            author: None,
            targets: vec![],
            mcp: vec![],
            sidecars: vec![],
            permissions: Permissions::default(),
            migrations: None,
            settings: None,
            ui: None,
            iyke: None,
            cron: vec![],
            window: None,
            queries: None,
            capabilities: None,
            engine: None,
            screenshots: vec![],
            requires: vec![],
            signature: None,
        }
    }

    #[test]
    fn normalize_is_order_insensitive_on_perm_vectors() {
        let mut a = minimal_manifest();
        a.permissions.shell_execute = vec!["bin/b".into(), "bin/a".into()];
        a.permissions.net = vec!["https://b/".into(), "https://a/".into()];

        let mut b = minimal_manifest();
        b.permissions.shell_execute = vec!["bin/a".into(), "bin/b".into()];
        b.permissions.net = vec!["https://a/".into(), "https://b/".into()];

        assert_eq!(normalize(&a), normalize(&b));
    }

    #[test]
    fn capabilities_changed_false_when_normalized_equal() {
        let m = minimal_manifest();
        let n = normalize(&m);
        assert!(!capabilities_changed(&n, &n));
    }

    #[test]
    fn capabilities_changed_true_when_perm_added() {
        let a = minimal_manifest();
        let mut b = minimal_manifest();
        b.permissions.fs_write.push("$home/Movies/**".into());
        assert!(capabilities_changed(&normalize(&a), &normalize(&b)));
    }

    #[test]
    fn capabilities_changed_true_when_perm_removed() {
        let mut a = minimal_manifest();
        a.permissions.shell_execute.push("bin/run".into());
        let b = minimal_manifest();
        assert!(capabilities_changed(&normalize(&a), &normalize(&b)));
    }

    #[test]
    fn capabilities_changed_true_when_capabilities_block_added() {
        let a = minimal_manifest();
        let mut b = minimal_manifest();
        b.capabilities = Some(crate::pkg::manifest::CapabilitiesBlock {
            supabase: Some(crate::pkg::manifest::SupabaseCapability { required: true }),
            sqlite: None,
            webview: None,
            agent_ops: None,
            http: None,
            secrets: None,
            invoke: None,
        });
        assert!(capabilities_changed(&normalize(&a), &normalize(&b)));
    }

    async fn open_test_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite");
        sqlx::query(
            "CREATE TABLE pkg_capability_snapshots (
                pkg_id                     TEXT PRIMARY KEY,
                manifest_capabilities_json TEXT NOT NULL,
                approved_at                INTEGER NOT NULL,
                approved_by_implicit       INTEGER NOT NULL DEFAULT 0
             )",
        )
        .execute(&pool)
        .await
        .expect("create pkg_capability_snapshots");
        pool
    }

    #[tokio::test]
    async fn fetch_returns_none_for_unknown_pkg() {
        let pool = open_test_pool().await;
        assert!(fetch(&pool, "com.absent.x").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn write_implicit_then_fetch_round_trip() {
        let pool = open_test_pool().await;
        let m = minimal_manifest();
        let json = normalize(&m);
        write_implicit(&pool, &m.id, &json).await.expect("write");
        let snap = fetch(&pool, &m.id).await.expect("fetch").expect("present");
        assert_eq!(snap.manifest_capabilities_json, json);
        assert!(snap.approved_by_implicit);
    }

    #[tokio::test]
    async fn write_explicit_marks_not_implicit_and_replaces_implicit_row() {
        let pool = open_test_pool().await;
        let m = minimal_manifest();
        let json = normalize(&m);
        write_implicit(&pool, &m.id, &json).await.expect("implicit");
        write_explicit(&pool, &m.id, &json).await.expect("explicit");
        let snap = fetch(&pool, &m.id).await.expect("fetch").expect("present");
        assert!(!snap.approved_by_implicit);
    }

    #[tokio::test]
    async fn write_explicit_replaces_old_snapshot_payload() {
        let pool = open_test_pool().await;
        let m_v1 = minimal_manifest();
        let json_v1 = normalize(&m_v1);
        write_implicit(&pool, &m_v1.id, &json_v1).await.expect("v1");

        let mut m_v2 = minimal_manifest();
        m_v2.permissions.shell_execute.push("bin/run".into());
        let json_v2 = normalize(&m_v2);
        write_explicit(&pool, &m_v2.id, &json_v2).await.expect("v2");

        let snap = fetch(&pool, &m_v1.id)
            .await
            .expect("fetch")
            .expect("present");
        assert_eq!(snap.manifest_capabilities_json, json_v2);
        assert!(!snap.approved_by_implicit);
    }

    #[tokio::test]
    async fn delete_removes_capability_snapshot() {
        let pool = open_test_pool().await;
        let m = minimal_manifest();
        let json = normalize(&m);
        write_implicit(&pool, &m.id, &json).await.expect("implicit");
        assert!(fetch(&pool, &m.id).await.unwrap().is_some());

        delete(&pool, &m.id).await.expect("delete");
        assert!(fetch(&pool, &m.id).await.unwrap().is_none());
    }
}
