//! Phase 9 — manifest trust gating.
//!
//! Pkgs that declare *sensitive* permissions (`shell.execute` non-empty, or
//! any `fs.write` glob that resolves outside the pkg's `$pkg_data` sandbox)
//! must be explicitly approved by the user before their MCP tools/call
//! invocations are honored. Everything else (skill packs, sandbox-only
//! writers, iframe-only UI, supabase-only readers) is auto-trusted on
//! install with no prompt.
//!
//! Storage piggybacks on `pkg_permissions_granted`. The trust grant is one
//! row per pkg with `scope_kind = '__manifest_trust'` (sentinel; never
//! collides with real capability scope kinds). `scope_value` is
//! `<sha256(snapshot_json)>` so a manifest update that changes the
//! sensitive perms invalidates the grant via mismatched value. The new
//! `version` column records the manifest version the grant was issued
//! against; `trust_state` toggles `granted` ↔ `revoked`.
//!
//! Auto-trust criterion (locked decision): pkg `source.kind = "builtin"`
//! AND `manifest.id` starts with `com.ikenga.`. We trust pkgs we ship; we
//! do not trust pkgs that merely *name* themselves like one of ours.
//!
//! Dev-mode bypass (2026-05-18): pkgs with `source.kind = "dev"` (mounted
//! via `ikenga dev <path>`) are also auto-trusted regardless of id
//! namespace. Sensitive perms still emit a `log::warn!` so the dev sees
//! what they've opted into, but no modal blocks the loop. Dev sources
//! never persist as `Dev` across reboots — boot reconstructs them as
//! plain `Local` — so this can't accidentally smuggle elevated trust
//! into a production install.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::pkg::manifest::{Package, Permissions};
use crate::pkg::signature::{self, SignatureVerdict};
use crate::pkg::source::InstallSource;

/// Sentinel scope_kind reserved for manifest trust grants. Never appears in
/// the manifest's declared `permissions.*` blocks; the permissions registry
/// only writes `fs.read`/`fs.write`/`shell.execute` rows.
const TRUST_SCOPE_KIND: &str = "__manifest_trust";

/// Sentinel scope_kind reserved for per-folder Studio project-access grants
/// (WP-04). Double-underscored + project-scoped so it never collides with a
/// real capability scope kind (`fs.read`/`fs.write`/`net`/…) nor with the
/// `__manifest_trust` sentinel above. `scope_value` is the **canonical**
/// filesystem path the user granted `com.ikenga.studio` access to.
pub const STUDIO_PROJECT_SCOPE_KIND: &str = "__studio_project";

/// The one pkg id that per-folder Studio grants are keyed to. The gate is
/// deliberately single-pkg — only the studio launcher / sidecar asks — so the
/// pkg id is a constant rather than a parameter.
pub const STUDIO_PKG_ID: &str = "com.ikenga.studio";

/// Plain-English summary of one declared permission set, for the trust
/// dialog and the `iyke_pkg_trust_*` MCP tools. Lists only the entries
/// that triggered the trust requirement, not the full perms block.
#[derive(Debug, Clone, Serialize)]
pub struct PermsSummary {
    pub shell_execute: Vec<String>,
    pub fs_write_outside_sandbox: Vec<String>,
    pub net: Vec<String>,
    pub vault_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NeedsApprovalReason {
    /// First time the user has seen this pkg.
    Never,
    /// Pkg was previously trusted but the sensitive-perms snapshot
    /// changed. `added` / `removed` are the diff against the prior grant.
    PermissionsChanged {
        prior_version: String,
        added: Vec<String>,
        removed: Vec<String>,
    },
    /// Existing grant explicitly revoked by the user.
    Revoked,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TrustState {
    /// Provenance-trusted, no user row required; never expires. Reached by:
    /// - `source.kind = "builtin"` AND id starts with `com.ikenga.`,
    /// - `source.kind = "dev"` (the `ikenga dev` bypass), OR
    /// - a `registry` source whose manifest `signature` minisign-verifies
    ///   against the install's `publisher_key` (WP-02 / `pkg::signature`).
    ///
    /// This is the only state that grants `is_trusted_for_elevated()` — the
    /// cryptographic / provenance anchor that elevated host caps
    /// (host.fetch / secrets / invoke) consult. A signed-registry pkg that
    /// reaches `AutoTrusted` is still subject to the sensitive-perms snapshot
    /// for `shell.execute` / unsandboxed `fs.write` (those follow the
    /// builtin's ceiling, i.e. covered by this same auto-trust).
    AutoTrusted,
    /// Pkg declares no sensitive perms — auto-granted on install with no
    /// prompt. Same surface as a user-approved row from the FE's POV.
    AutoGranted,
    /// User explicitly approved this version's sensitive perms.
    Granted { version: String, granted_at_ms: i64 },
    /// Approval needed. Caller must surface the prompt.
    NeedsApproval {
        reason: NeedsApprovalReason,
        current_version: String,
    },
}

impl TrustState {
    /// True when this state allows MCP tools/call to proceed.
    pub fn is_allowed(&self) -> bool {
        matches!(
            self,
            TrustState::AutoTrusted | TrustState::AutoGranted | TrustState::Granted { .. }
        )
    }

    /// The SINGLE gate for *elevated* host capabilities (host.fetch,
    /// secrets/named-secret injection, host.invoke — consumed by WP-03/04/05).
    ///
    /// Returns `true` ONLY for `AutoTrusted`, i.e. a pkg that earned trust by
    /// **provenance**: builtin-in-namespace, a `ikenga dev` mount, or a
    /// registry pkg whose manifest signature cryptographically verified
    /// (`pkg::signature::verify_manifest_signature(...).is_valid()`, wired in
    /// `evaluate()` below).
    ///
    /// It deliberately returns `false` for `AutoGranted` and `Granted`. Those
    /// states mean the pkg either declares no sensitive perms (`AutoGranted`)
    /// or a *user clicked approve* on its sensitive-perms snapshot
    /// (`Granted`). A user approving `shell.execute` / `fs.write` is **not**
    /// the same as provenance trust: approving a community pkg's declared
    /// perms lets its MCP tools run, but it does NOT hand that pkg the
    /// cryptographic anchor that elevated host caps require. Elevated caps are
    /// gated on *who published this and did the bytes verify*, not on *did the
    /// user consent to the declared perms*. The two are intentionally
    /// orthogonal: `is_allowed()` (run at all) vs `is_trusted_for_elevated()`
    /// (reach the privileged host surface).
    pub fn is_trusted_for_elevated(&self) -> bool {
        matches!(self, TrustState::AutoTrusted)
    }

    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            TrustState::AutoTrusted => "auto_trusted",
            TrustState::AutoGranted => "auto_granted",
            TrustState::Granted { .. } => "granted",
            TrustState::NeedsApproval { .. } => "needs_approval",
        }
    }
}

/// Stable wire form for a sensitive-perms snapshot. The hex sha256 of this
/// string is what we store in `scope_value`; mismatch ⇒ permissions changed.
fn snapshot_json(perms: &PermsSummary) -> String {
    // Sort each list so reordering doesn't invalidate trust.
    let mut shell = perms.shell_execute.clone();
    let mut fsw = perms.fs_write_outside_sandbox.clone();
    let mut net = perms.net.clone();
    let mut vk = perms.vault_keys.clone();
    shell.sort();
    fsw.sort();
    net.sort();
    vk.sort();
    serde_json::to_string(&serde_json::json!({
        "shell_execute": shell,
        "fs_write_outside_sandbox": fsw,
        "net": net,
        "vault_keys": vk,
    }))
    .expect("snapshot json")
}

fn snapshot_hash(perms: &PermsSummary) -> String {
    let mut hasher = Sha256::new();
    hasher.update(snapshot_json(perms).as_bytes());
    hex::encode(hasher.finalize())
}

/// Resolve `$pkg_data` for a given pkg id under the supplied app data root.
/// Mirrors the path computed by `permissions::resolve_path` so the "outside
/// sandbox" check stays consistent with what the kernel actually grants.
fn pkg_data_dir(app_data_dir: &PathBuf, pkg_id: &str) -> PathBuf {
    app_data_dir.join("pkgs").join(pkg_id).join("data")
}

/// True when this raw `fs.write` glob resolves to a path outside the pkg's
/// `$pkg_data` sandbox. `$pkg_data` and `$pkg_install` are sandbox-aligned
/// (kernel-managed); anything else (`$home`, absolute paths) is sensitive.
fn fs_write_is_sensitive(raw: &str) -> bool {
    !raw.starts_with("$pkg_data") && !raw.starts_with("$pkg_install")
}

/// Compute the sensitive-perms summary for a manifest. Only the fields that
/// actually triggered the trust requirement get populated; everything else
/// is empty.
pub fn summarize_sensitive(perms: &Permissions) -> PermsSummary {
    let fs_write_outside_sandbox = perms
        .fs_write
        .iter()
        .filter(|raw| fs_write_is_sensitive(raw))
        .cloned()
        .collect::<Vec<_>>();
    PermsSummary {
        shell_execute: perms.shell_execute.clone(),
        fs_write_outside_sandbox,
        net: perms.net.clone(),
        vault_keys: perms.vault_keys.clone(),
    }
}

/// True when the manifest declares perms that require explicit user trust.
pub fn requires_trust(perms: &Permissions) -> bool {
    let s = summarize_sensitive(perms);
    !s.shell_execute.is_empty() || !s.fs_write_outside_sandbox.is_empty()
}

/// Same as `pkg_id.starts_with("com.ikenga.")` — but explicit so the call
/// site reads as the policy decision, not a string check.
fn id_in_ikenga_namespace(pkg_id: &str) -> bool {
    pkg_id.starts_with("com.ikenga.")
}

/// Auto-trust policy:
///
/// - `source.kind = "builtin"` AND id in `com.ikenga.*` namespace, OR
/// - `source.kind = "dev"` — dev-mode mounts via `ikenga dev` bypass the
///   modal regardless of pkg id (sensitive perms still log::warn).
///
/// Neither path alone is insufficient for the builtin case: an attacker
/// who got their pkg into `builtin-pkgs/` still can't masquerade as
/// `com.ikenga.*`, and a sideloaded pkg that names itself `com.ikenga.X`
/// doesn't count.
pub fn is_auto_trusted(pkg_id: &str, source: &InstallSource) -> bool {
    if source.is_dev() {
        return true;
    }
    source.is_builtin() && id_in_ikenga_namespace(pkg_id)
}

/// Pull the latest trust row for a pkg, if any.
async fn fetch_trust_row(
    pool: &SqlitePool,
    pkg_id: &str,
) -> Result<Option<(String, String, String, i64)>> {
    // (version, scope_value (hash), trust_state, granted_at)
    let row: Option<(Option<String>, String, String, i64)> = sqlx::query_as(
        "SELECT version, scope_value, trust_state, granted_at
           FROM pkg_permissions_granted
          WHERE pkg_id = ? AND scope_kind = ?
          ORDER BY granted_at DESC
          LIMIT 1",
    )
    .bind(pkg_id)
    .bind(TRUST_SCOPE_KIND)
    .fetch_optional(pool)
    .await
    .context("read pkg_permissions_granted (trust)")?;
    Ok(row.map(|(v, s, st, t)| (v.unwrap_or_default(), s, st, t)))
}

/// Evaluate the trust state for an installed pkg.
///
/// Pure function over the supplied inputs — no Tauri / app-handle reads. The
/// caller threads in `app_data_dir` so the "outside sandbox" calculation is
/// deterministic regardless of test harness.
pub async fn evaluate(
    pool: &SqlitePool,
    pkg: &Package,
    source: &InstallSource,
    app_data_dir: &PathBuf,
) -> Result<TrustState> {
    let pkg_id = &pkg.manifest.id;
    let version = pkg.manifest.version.clone();

    if is_auto_trusted(pkg_id, source) {
        // Dev-mode trust bypass: surface the elevated perms in the log so the
        // dev sees what they've opted into without a modal blocking the loop.
        // Sticks at WARN so the line is hard to miss in the shell's stderr.
        if source.is_dev() {
            let perms = &pkg.manifest.permissions;
            if requires_trust(perms) {
                let s = summarize_sensitive(perms);
                log::warn!(
                    "[pkg_trust] dev pkg `{pkg_id}` auto-trusted with sensitive perms: \
                     shell.execute={:?} fs.write_outside_sandbox={:?}",
                    s.shell_execute,
                    s.fs_write_outside_sandbox
                );
            }
        }
        return Ok(TrustState::AutoTrusted);
    }

    // Signed-registry provenance (WP-02 / G-TRUST). A registry pkg whose
    // manifest `signature` minisign-verifies against the install's
    // `publisher_key` earns the same provenance anchor as a builtin:
    // `AutoTrusted`, which carries `is_trusted_for_elevated()`. The verdict is
    // re-derived from the raw `manifest.json` on disk on every call, so a pkg
    // whose bytes changed after install (or whose key/sig is wrong) drops back
    // to `Invalid` and never reaches this arm — fail-closed by construction.
    //
    // Subtlety vs. the sensitive-perms snapshot: we only promote to
    // `AutoTrusted` here when the pkg declares **no** sensitive perms
    // (`!requires_trust`). A signed pkg that ALSO wants `shell.execute` or
    // unsandboxed `fs.write` falls through to the snapshot/approval flow
    // below — the signature does NOT silently auto-grant those; the user still
    // approves them (yielding `Granted`, which is intentionally not
    // elevated-trusted). Anything other than a clean `Valid` verdict
    // (`Unsigned` / `MissingPublisherKey` / `Invalid`) is a no-op here: the
    // pkg evaluates exactly as it would today.
    if source.is_registry() {
        let verdict = signature::verify_manifest_signature(pkg, source);
        match &verdict {
            SignatureVerdict::Valid => {
                tracing::info!(
                    "[pkg_trust] registry pkg `{pkg_id}` signature verdict={} → provenance-trusted",
                    verdict.label()
                );
                if !requires_trust(&pkg.manifest.permissions) {
                    return Ok(TrustState::AutoTrusted);
                }
                // Signed AND sensitive: keep the elevated anchor but still
                // require explicit approval of the sensitive perms. Fall
                // through to the snapshot flow.
            }
            // Unsigned / missing-key / invalid: log at the right level and let
            // the normal (untrusted) evaluation proceed. Never grants trust.
            SignatureVerdict::Unsigned | SignatureVerdict::NotApplicable => {
                tracing::info!(
                    "[pkg_trust] registry pkg `{pkg_id}` signature verdict={} → not provenance-trusted",
                    verdict.label()
                );
            }
            SignatureVerdict::MissingPublisherKey | SignatureVerdict::Invalid { .. } => {
                tracing::warn!(
                    "[pkg_trust] registry pkg `{pkg_id}` signature verdict={} → NOT provenance-trusted (fail-closed)",
                    verdict.label()
                );
            }
        }
    }

    let perms = &pkg.manifest.permissions;
    if !requires_trust(perms) {
        return Ok(TrustState::AutoGranted);
    }

    let summary = summarize_sensitive(perms);
    let _sandbox = pkg_data_dir(app_data_dir, pkg_id); // currently unused; reserved for future absolute-path normalization
    let current_hash = snapshot_hash(&summary);

    let Some((row_version, row_hash, trust_state, granted_at)) =
        fetch_trust_row(pool, pkg_id).await?
    else {
        return Ok(TrustState::NeedsApproval {
            reason: NeedsApprovalReason::Never,
            current_version: version,
        });
    };

    if trust_state == "revoked" {
        return Ok(TrustState::NeedsApproval {
            reason: NeedsApprovalReason::Revoked,
            current_version: version,
        });
    }

    if row_hash == current_hash {
        return Ok(TrustState::Granted {
            version: row_version,
            granted_at_ms: granted_at,
        });
    }

    // Hash mismatch — diff the prior summary against current. We don't
    // re-store the prior summary; instead derive added/removed by parsing
    // the prior row's hash isn't possible, so we fall back to "the perms
    // changed since version <prior>" without a granular diff. The dialog
    // shows the full current set highlighted as new.
    Ok(TrustState::NeedsApproval {
        reason: NeedsApprovalReason::PermissionsChanged {
            prior_version: row_version,
            added: vec![],
            removed: vec![],
        },
        current_version: version,
    })
}

/// Insert a granted row capturing the current sensitive-perms snapshot.
/// Idempotent under PRIMARY KEY `(pkg_id, scope_kind, scope_value)` —
/// re-granting the same hash silently bumps `granted_at`.
pub async fn grant(
    pool: &SqlitePool,
    pkg_id: &str,
    version: &str,
    summary: &PermsSummary,
) -> Result<()> {
    // Mark prior rows revoked first so a subsequent evaluate() reads the new
    // row by `granted_at DESC`.
    sqlx::query(
        "UPDATE pkg_permissions_granted
            SET trust_state = 'revoked'
          WHERE pkg_id = ? AND scope_kind = ? AND trust_state = 'granted'",
    )
    .bind(pkg_id)
    .bind(TRUST_SCOPE_KIND)
    .execute(pool)
    .await
    .context("revoke prior trust rows before grant")?;

    let hash = snapshot_hash(summary);
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT OR REPLACE INTO pkg_permissions_granted
            (pkg_id, scope_kind, scope_value, granted_at, version, trust_state)
            VALUES (?, ?, ?, ?, ?, 'granted')",
    )
    .bind(pkg_id)
    .bind(TRUST_SCOPE_KIND)
    .bind(&hash)
    .bind(now)
    .bind(version)
    .execute(pool)
    .await
    .context("insert pkg_permissions_granted (trust)")?;
    Ok(())
}

/// True when `com.ikenga.studio` already holds a granted per-folder access
/// row for `canonical_path` (WP-04). Constant-time single-row lookup keyed by
/// the PRIMARY KEY `(pkg_id, scope_kind, scope_value)`, so a re-hit on an
/// already-granted folder is O(1) and prompts nothing. `canonical_path` MUST
/// be the canonicalized form (see `commands::pkg_studio::canonicalize`); the
/// grant row stores canonical paths, so a raw/uncanonicalized lookup would
/// spuriously miss.
pub async fn has_studio_project_grant(pool: &SqlitePool, canonical_path: &str) -> Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT 1
           FROM pkg_permissions_granted
          WHERE pkg_id = ? AND scope_kind = ? AND scope_value = ? AND trust_state = 'granted'
          LIMIT 1",
    )
    .bind(STUDIO_PKG_ID)
    .bind(STUDIO_PROJECT_SCOPE_KIND)
    .bind(canonical_path)
    .fetch_optional(pool)
    .await
    .context("read studio project grant")?;
    Ok(row.is_some())
}

/// Record a per-folder Studio access grant for `canonical_path` (WP-04).
/// Idempotent under the PRIMARY KEY `(pkg_id, scope_kind, scope_value)` —
/// re-granting the same folder is `INSERT OR REPLACE`, which bumps
/// `granted_at` and leaves exactly one row. `version` is unused for
/// folder-scoped grants (they don't invalidate on manifest bumps the way the
/// sensitive-perms snapshot does), so it is stored NULL.
pub async fn record_studio_project_grant(pool: &SqlitePool, canonical_path: &str) -> Result<()> {
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT OR REPLACE INTO pkg_permissions_granted
            (pkg_id, scope_kind, scope_value, granted_at, version, trust_state)
            VALUES (?, ?, ?, ?, NULL, 'granted')",
    )
    .bind(STUDIO_PKG_ID)
    .bind(STUDIO_PROJECT_SCOPE_KIND)
    .bind(canonical_path)
    .bind(now)
    .execute(pool)
    .await
    .context("insert studio project grant")?;
    Ok(())
}

/// Mark all granted rows for this pkg as revoked. Subsequent evaluate()
/// returns NeedsApproval { reason: Revoked }.
pub async fn revoke(pool: &SqlitePool, pkg_id: &str) -> Result<()> {
    sqlx::query(
        "UPDATE pkg_permissions_granted
            SET trust_state = 'revoked'
          WHERE pkg_id = ? AND scope_kind = ? AND trust_state = 'granted'",
    )
    .bind(pkg_id)
    .bind(TRUST_SCOPE_KIND)
    .execute(pool)
    .await
    .context("revoke trust rows")?;
    Ok(())
}

/// Resolve whether an installed pkg is trusted enough to hold an **elevated**
/// host capability (host.fetch / named-secret injection / host.invoke). This is
/// the shared seam every Wave-2 elevated verb consults, factored out of the
/// `pkg_mcp.rs` recipe so the secret-injection path (`pkg_content_html`) and the
/// `pkg_is_trusted_for_elevated` Tauri command (reused by WP-04/05) agree on a
/// single evaluation:
///
/// 1. Re-load the manifest off disk (`Package::load`) so the verdict reflects
///    the bytes actually installed (signature re-verification is disk-fresh).
/// 2. Resolve the pkg's `InstallSource` from the kernel's installed summary
///    (falling back to `Local` if absent, matching `pkg_mcp.rs`).
/// 3. `trust::evaluate(...)` → `TrustState::is_trusted_for_elevated()`.
///
/// **Fail-closed:** an un-installed pkg, a manifest that won't load, or any
/// trust-evaluation error all resolve to `false` — never a panic, never an
/// accidental grant.
pub async fn resolve_elevated_trust(
    pool: &SqlitePool,
    kernel: &crate::pkg::kernel::Kernel,
    app_data_dir: &PathBuf,
    pkg_id: &str,
) -> bool {
    let Some(install_path) = kernel.installed_path(pkg_id) else {
        return false;
    };
    let pkg = match Package::load(&install_path) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "[pkg_trust] resolve_elevated_trust: pkg `{pkg_id}` manifest reload failed: {e:#}"
            );
            return false;
        }
    };
    let source = kernel
        .installed_summary(pkg_id)
        .map(|s| s.source)
        .unwrap_or(InstallSource::Local {
            path: install_path.display().to_string(),
        });
    match evaluate(pool, &pkg, &source, app_data_dir).await {
        Ok(state) => state.is_trusted_for_elevated(),
        Err(e) => {
            tracing::warn!(
                "[pkg_trust] resolve_elevated_trust: pkg `{pkg_id}` evaluate failed: {e:#}"
            );
            false
        }
    }
}

/// Convenience: structured error returned by MCP call sites when a tools/call
/// hits an untrusted pkg. The string form is what propagates through the
/// existing `anyhow!` plumbing; FE / agents parse the prefix to detect the
/// trust-required case.
pub fn trust_required_error(pkg_id: &str) -> anyhow::Error {
    anyhow!(
        "trust_required: pkg `{pkg_id}` is awaiting user approval — \
         grant via Settings → Pkgs → Trust"
    )
}

/// True when an `anyhow::Error` was produced by `trust_required_error`. Used
/// by the supervisor surface to translate into a `LifecycleKind::Error`.
#[allow(dead_code)]
pub fn is_trust_required_error(e: &anyhow::Error) -> bool {
    e.to_string().starts_with("trust_required:")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::manifest::{Manifest, Permissions};
    use std::path::PathBuf;

    fn pkg_with_perms(id: &str, perms: Permissions) -> Package {
        Package {
            manifest: Manifest {
                id: id.into(),
                name: "T".into(),
                version: "0.1.0".into(),
                ikenga_api: "1".into(),
                kind: None,
                auth_bridge: None,
            author: None,
                targets: vec![],
                mcp: vec![],
                sidecars: vec![],
                permissions: perms,
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
            },
            install_path: PathBuf::from("/tmp"),
        }
    }

    // ── G-TRUST: signed-registry provenance truth table ──────────────────────
    //
    // These exercise the WP-02 wiring end-to-end: `evaluate()` calls
    // `signature::verify_manifest_signature`, which reads the **raw**
    // manifest.json off disk and minisign-verifies it — independent of the
    // in-memory `Manifest` struct. So each fixture writes the full golden
    // manifest text (the WP-06 contract shape, which the current Rust struct
    // doesn't model field-for-field — `capabilities.http` etc.) into a tempdir
    // and constructs a `Package` whose `install_path` points there. The
    // signature check sees the exact golden bytes on disk; the in-memory
    // `Manifest` only needs the fields trust.rs reads (id / version / perms /
    // signature). This mirrors how `signature.rs` only ever parses the golden
    // as a generic `serde_json::Value`, never via `Package::load`.

    // The shared golden vector (same artifacts `signature.rs` tests against).
    const GOLDEN_MANIFEST: &str =
        include_str!("testdata/signature_golden_v1/manifest.json");
    const GOLDEN_PUBKEY: &str = include_str!("testdata/signature_golden_v1/publisher.pub");
    const GOLDEN_SIG: &str = include_str!("testdata/signature_golden_v1/manifest.minisig");

    /// Build the golden `Permissions` (net + vault.keys only — NO sensitive
    /// perms, so `requires_trust` is false; matches the golden manifest.json).
    fn golden_perms() -> Permissions {
        let mut p = Permissions::default();
        p.net.push("https://api.example.com/".into());
        p.vault_keys.push("EXAMPLE_API_KEY".into());
        p
    }

    /// Write `on_disk_text` (the raw manifest.json the signature check reads)
    /// into a fresh tempdir and build a `Package` rooted there. `signature` is
    /// the in-memory `Manifest.signature` (the `verify_manifest_signature`
    /// short-circuit checks it before touching disk). `perms` drives the
    /// `requires_trust` branch. The TempDir is returned so the caller keeps it
    /// alive (Package only holds the path).
    fn pkg_on_disk(
        on_disk_text: &str,
        signature: Option<String>,
        perms: Permissions,
    ) -> (Package, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("manifest.json"), on_disk_text)
            .expect("write manifest.json");
        let mut pkg = pkg_with_perms("com.example.signed", perms);
        pkg.manifest.version = "1.2.3".into();
        pkg.manifest.signature = signature;
        pkg.install_path = dir.path().to_path_buf();
        (pkg, dir)
    }

    /// The golden manifest text with its placeholder `signature` replaced by the
    /// real golden `.minisig` blob (JSON-escaped). Canonicalization strips the
    /// signature field, so the canonical bytes are identical to the golden
    /// vector either way — the embedded blob is what gets verified.
    fn golden_manifest_with_real_sig() -> String {
        let sig_json = serde_json::to_string(GOLDEN_SIG).expect("escape sig");
        GOLDEN_MANIFEST.replace(
            r#""signature": "PLACEHOLDER_TO_BE_STRIPPED""#,
            &format!(r#""signature": {sig_json}"#),
        )
    }

    fn registry_source_with_golden_key() -> InstallSource {
        InstallSource::Registry {
            url: "https://reg.example/r".into(),
            publisher_key: Some(GOLDEN_PUBKEY.to_string()),
        }
    }

    /// valid-signature registry pkg → AutoTrusted + is_trusted_for_elevated.
    #[tokio::test]
    async fn g_trust_valid_signature_registry_is_auto_trusted_and_elevated() {
        let pool = open_test_pool().await;
        let (pkg, _dir) = pkg_on_disk(
            &golden_manifest_with_real_sig(),
            Some(GOLDEN_SIG.to_string()),
            golden_perms(),
        );
        let state = evaluate(
            &pool,
            &pkg,
            &registry_source_with_golden_key(),
            &PathBuf::from("/tmp"),
        )
        .await
        .expect("evaluate");
        assert!(
            matches!(state, TrustState::AutoTrusted),
            "valid-signature registry pkg must reach AutoTrusted, got {state:?}"
        );
        assert!(state.is_trusted_for_elevated());
        assert!(state.is_allowed());
    }

    /// unsigned registry pkg → NOT auto-trusted, NOT elevated (but runs fine).
    #[tokio::test]
    async fn g_trust_unsigned_registry_runs_but_not_elevated() {
        let pool = open_test_pool().await;
        // Golden manifest with the `signature` field removed entirely.
        let unsigned = GOLDEN_MANIFEST.replace(
            ",\n  \"signature\": \"PLACEHOLDER_TO_BE_STRIPPED\"",
            "",
        );
        assert!(!unsigned.contains("signature"), "sig must be gone");
        let (pkg, _dir) = pkg_on_disk(&unsigned, None, golden_perms());
        let state = evaluate(
            &pool,
            &pkg,
            &registry_source_with_golden_key(),
            &PathBuf::from("/tmp"),
        )
        .await
        .expect("evaluate");
        // No sensitive perms (golden has empty shell.execute/fs.write) →
        // AutoGranted: runs, but is NOT provenance/elevated-trusted.
        assert!(matches!(state, TrustState::AutoGranted), "got {state:?}");
        assert!(state.is_allowed(), "unsigned pkg still runs");
        assert!(
            !state.is_trusted_for_elevated(),
            "unsigned pkg must NOT be elevated-trusted"
        );
    }

    /// tampered manifest (bytes changed after signing) → NOT trusted, NOT
    /// elevated. Proves disk-fresh re-verification catches post-install edits.
    #[tokio::test]
    async fn g_trust_tampered_manifest_registry_not_trusted() {
        let pool = open_test_pool().await;
        // Keep the real signature, but mutate a signed field (name) so the
        // canonical bytes no longer match what was signed.
        let tampered = golden_manifest_with_real_sig()
            .replace(r#""name": "Signed Example""#, r#""name": "Tampered Evil""#);
        let (pkg, _dir) = pkg_on_disk(&tampered, Some(GOLDEN_SIG.to_string()), golden_perms());
        let state = evaluate(
            &pool,
            &pkg,
            &registry_source_with_golden_key(),
            &PathBuf::from("/tmp"),
        )
        .await
        .expect("evaluate");
        assert!(
            !state.is_trusted_for_elevated(),
            "tampered manifest must NOT be elevated-trusted, got {state:?}"
        );
        // Still runs (no sensitive perms) — tampering the elevated anchor
        // doesn't brick the pkg, it just denies elevated caps.
        assert!(matches!(state, TrustState::AutoGranted), "got {state:?}");
    }

    /// wrong publisher key → NOT trusted, NOT elevated.
    #[tokio::test]
    async fn g_trust_wrong_publisher_key_registry_not_trusted() {
        let pool = open_test_pool().await;
        let (pkg, _dir) = pkg_on_disk(
            &golden_manifest_with_real_sig(),
            Some(GOLDEN_SIG.to_string()),
            golden_perms(),
        );
        // A real, valid minisign key that simply didn't sign our content.
        const OTHER_KEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        let source = InstallSource::Registry {
            url: "u".into(),
            publisher_key: Some(OTHER_KEY.into()),
        };
        let state = evaluate(&pool, &pkg, &source, &PathBuf::from("/tmp"))
            .await
            .expect("evaluate");
        assert!(
            !state.is_trusted_for_elevated(),
            "wrong key must NOT be elevated-trusted, got {state:?}"
        );
    }

    /// signed manifest but the index named no publisher_key → fail-closed,
    /// NOT elevated.
    #[tokio::test]
    async fn g_trust_missing_publisher_key_registry_not_trusted() {
        let pool = open_test_pool().await;
        let (pkg, _dir) = pkg_on_disk(
            &golden_manifest_with_real_sig(),
            Some(GOLDEN_SIG.to_string()),
            golden_perms(),
        );
        let source = InstallSource::Registry {
            url: "u".into(),
            publisher_key: None,
        };
        let state = evaluate(&pool, &pkg, &source, &PathBuf::from("/tmp"))
            .await
            .expect("evaluate");
        assert!(
            !state.is_trusted_for_elevated(),
            "missing key must NOT be elevated-trusted, got {state:?}"
        );
    }

    /// builtin-in-namespace → is_trusted_for_elevated (no signature needed).
    #[tokio::test]
    async fn g_trust_builtin_in_namespace_is_elevated_without_signature() {
        let pool = open_test_pool().await;
        let pkg = pkg_with_perms("com.ikenga.iyke", Permissions::default());
        let state = evaluate(&pool, &pkg, &InstallSource::Builtin, &PathBuf::from("/tmp"))
            .await
            .expect("evaluate");
        assert!(matches!(state, TrustState::AutoTrusted));
        assert!(state.is_trusted_for_elevated());
    }

    /// THE key distinction: a user-Granted (sensitive-perms-approved) but
    /// UNSIGNED registry pkg is allowed to run, but is NOT elevated-trusted.
    /// User approval ≠ provenance/signature trust.
    #[tokio::test]
    async fn g_trust_user_granted_unsigned_is_allowed_but_not_elevated() {
        let pool = open_test_pool().await;
        // Use the seeded com.evil.studio row; give it a sensitive perm.
        let mut perms = Permissions::default();
        perms.shell_execute.push("bin/run".into());
        let pkg = pkg_with_perms("com.evil.studio", perms);
        let source = InstallSource::Registry {
            url: "u".into(),
            publisher_key: None,
        };

        // User approves the sensitive-perms snapshot.
        let summary = summarize_sensitive(&pkg.manifest.permissions);
        grant(&pool, &pkg.manifest.id, &pkg.manifest.version, &summary)
            .await
            .expect("grant");

        let state = evaluate(&pool, &pkg, &source, &PathBuf::from("/tmp"))
            .await
            .expect("evaluate");
        assert!(matches!(state, TrustState::Granted { .. }), "got {state:?}");
        assert!(state.is_allowed(), "user-granted pkg runs");
        assert!(
            !state.is_trusted_for_elevated(),
            "user approval is NOT provenance trust — must NOT be elevated"
        );
    }

    /// WP-05 D-06 — THE key assertion. A signed/builtin pkg that declares
    /// `capabilities.invoke.commands` but leaves `permissions["shell.execute"]`
    /// EMPTY reaches `AutoTrusted` → `is_trusted_for_elevated()` is true. This is
    /// the whole reason the invoke allowlist is its OWN field and NOT
    /// `shell.execute`: a non-empty `shell.execute` would trip `requires_trust`,
    /// dropping the pkg to user-`Granted` (never `AutoTrusted`), at which point
    /// `host.invoke` could never run. By keeping the command allowlist in
    /// `capabilities.invoke.commands`, `shell.execute` stays empty, `requires_trust`
    /// is false, and a builtin (or signed registry) pkg stays elevated-trusted.
    #[tokio::test]
    async fn d06_builtin_with_invoke_commands_but_empty_shell_execute_is_elevated() {
        use crate::pkg::manifest::{CapabilitiesBlock, InvokeCapability};
        let pool = open_test_pool().await;

        // Empty shell.execute → requires_trust(false). Declare an invoke
        // allowlist in the cap instead.
        let mut pkg = pkg_with_perms("com.ikenga.outbound", Permissions::default());
        pkg.manifest.capabilities = Some(CapabilitiesBlock {
            invoke: Some(InvokeCapability {
                commands: vec!["pa_actions_commit".into(), "pa_actions_reject".into()],
            }),
            ..Default::default()
        });
        // Sanity: the allowlist living in the cap does NOT make shell.execute
        // non-empty, so requires_trust stays false.
        assert!(
            !requires_trust(&pkg.manifest.permissions),
            "invoke.commands must NOT trip requires_trust (it's not shell.execute)"
        );

        let state = evaluate(&pool, &pkg, &InstallSource::Builtin, &PathBuf::from("/tmp"))
            .await
            .expect("evaluate");
        assert!(
            matches!(state, TrustState::AutoTrusted),
            "builtin pkg w/ invoke.commands + empty shell.execute must be AutoTrusted, got {state:?}"
        );
        assert!(
            state.is_trusted_for_elevated(),
            "the D-06 fix: this pkg MUST reach is_trusted_for_elevated() == true"
        );

        // Counterfactual: had the allowlist been expressed as shell.execute, the
        // SAME pkg would trip requires_trust and (as a registry/local pkg) drop
        // to NeedsApproval — never elevated. Proves WHY the field is separate.
        let mut counter = pkg_with_perms("com.evil.studio", {
            let mut p = Permissions::default();
            p.shell_execute.push("pa_actions_commit".into());
            p
        });
        counter.manifest.capabilities = None;
        assert!(
            requires_trust(&counter.manifest.permissions),
            "shell.execute non-empty DOES trip requires_trust"
        );
        let counter_state = evaluate(
            &pool,
            &counter,
            &InstallSource::Local { path: "/tmp".into() },
            &PathBuf::from("/tmp"),
        )
        .await
        .expect("evaluate");
        assert!(
            !counter_state.is_trusted_for_elevated(),
            "a shell.execute-gated pkg is NOT elevated — this is the trap D-06 avoids"
        );
    }

    #[test]
    fn auto_trust_requires_both_builtin_source_and_ikenga_id() {
        // Both ✓ → auto-trust.
        assert!(is_auto_trusted("com.ikenga.iyke", &InstallSource::Builtin));
        // Builtin source, third-party id → NOT auto-trusted (an attacker
        // who got their pkg into builtin-pkgs/ still can't masquerade).
        assert!(!is_auto_trusted("com.evil.miner", &InstallSource::Builtin));
        // ikenga id, non-builtin source → NOT auto-trusted (a sideloaded
        // pkg that names itself com.ikenga.X doesn't count).
        assert!(!is_auto_trusted(
            "com.ikenga.evil",
            &InstallSource::Local {
                path: "/tmp".into()
            }
        ));
        // Neither → NOT auto-trusted.
        assert!(!is_auto_trusted(
            "com.royalti.studio",
            &InstallSource::Registry {
                url: "u".into(),
                publisher_key: None,
            }
        ));
    }

    #[test]
    fn dev_source_auto_trusts_regardless_of_id_namespace() {
        // Dev mode bypasses the com.ikenga.* requirement — a developer's
        // `com.example.thing` symlinked via `ikenga dev` is still trusted.
        assert!(is_auto_trusted(
            "com.example.dashboard",
            &InstallSource::Dev {
                path: "/home/me/code/dashboard".into()
            }
        ));
        // And a dev pkg using the reserved namespace is also fine — devs
        // building first-party builtins should be able to test them this way.
        assert!(is_auto_trusted(
            "com.ikenga.studio",
            &InstallSource::Dev {
                path: "/home/me/code/studio".into()
            }
        ));
        // Local (non-dev) still requires both source AND namespace as before.
        assert!(!is_auto_trusted(
            "com.example.dashboard",
            &InstallSource::Local {
                path: "/home/me/code/dashboard".into()
            }
        ));
    }

    #[test]
    fn fs_write_is_sandbox_only_when_under_pkg_placeholders() {
        assert!(!fs_write_is_sensitive("$pkg_data/cache/**"));
        assert!(!fs_write_is_sensitive("$pkg_install/templates/**"));
        assert!(fs_write_is_sensitive("$home/Movies/Ikenga/**"));
        assert!(fs_write_is_sensitive("/etc/passwd"));
    }

    #[test]
    fn requires_trust_skips_skill_packs() {
        let perms = Permissions::default();
        assert!(!requires_trust(&perms));
    }

    #[test]
    fn requires_trust_fires_on_shell_execute() {
        let mut perms = Permissions::default();
        perms.shell_execute.push("bin/run".into());
        assert!(requires_trust(&perms));
    }

    #[test]
    fn requires_trust_fires_on_unsandboxed_fs_write_only() {
        let mut perms = Permissions::default();
        perms.fs_write.push("$pkg_data/cache/**".into());
        // Sandbox-only writers are NOT sensitive.
        assert!(!requires_trust(&perms));
        perms.fs_write.push("$home/Movies/Ikenga/**".into());
        assert!(requires_trust(&perms));
    }

    #[test]
    fn requires_trust_does_not_fire_on_net_alone() {
        // Decision: `net` alone doesn't trigger the prompt (read decisions
        // log: "Any non-empty shell.execute, fs.write outside $pkg_data").
        // It's still surfaced in the dialog when a prompt fires for
        // another reason, but it's not a trigger by itself.
        let mut perms = Permissions::default();
        perms.net.push("https://api.openai.com/".into());
        assert!(!requires_trust(&perms));
    }

    #[test]
    fn snapshot_hash_is_order_insensitive() {
        let a = PermsSummary {
            shell_execute: vec!["b".into(), "a".into()],
            fs_write_outside_sandbox: vec![],
            net: vec![],
            vault_keys: vec![],
        };
        let b = PermsSummary {
            shell_execute: vec!["a".into(), "b".into()],
            fs_write_outside_sandbox: vec![],
            net: vec![],
            vault_keys: vec![],
        };
        assert_eq!(snapshot_hash(&a), snapshot_hash(&b));
    }

    #[test]
    fn snapshot_hash_changes_when_perms_change() {
        let a = PermsSummary {
            shell_execute: vec!["bin/run".into()],
            fs_write_outside_sandbox: vec![],
            net: vec![],
            vault_keys: vec![],
        };
        let b = PermsSummary {
            shell_execute: vec!["bin/run".into(), "scripts/sh".into()],
            fs_write_outside_sandbox: vec![],
            net: vec![],
            vault_keys: vec![],
        };
        assert_ne!(snapshot_hash(&a), snapshot_hash(&b));
    }

    async fn open_test_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite");
        // Minimum schema: parent table + the trust columns we use.
        sqlx::query(
            "CREATE TABLE pkg_installed (
                id TEXT PRIMARY KEY,
                version TEXT NOT NULL,
                ikenga_api TEXT NOT NULL,
                manifest_json TEXT NOT NULL,
                install_path TEXT NOT NULL,
                installed_at INTEGER NOT NULL,
                enabled INTEGER NOT NULL,
                source_json TEXT NOT NULL,
                project_id TEXT
             )",
        )
        .execute(&pool)
        .await
        .expect("create pkg_installed");
        sqlx::query(
            "CREATE TABLE pkg_permissions_granted (
                pkg_id TEXT NOT NULL,
                scope_kind TEXT NOT NULL,
                scope_value TEXT NOT NULL,
                granted_at INTEGER NOT NULL,
                version TEXT,
                trust_state TEXT NOT NULL DEFAULT 'granted',
                PRIMARY KEY (pkg_id, scope_kind, scope_value)
             )",
        )
        .execute(&pool)
        .await
        .expect("create pkg_permissions_granted");
        sqlx::query(
            "INSERT INTO pkg_installed
              (id, version, ikenga_api, manifest_json, install_path, installed_at, enabled, source_json, project_id)
             VALUES ('com.evil.studio', '0.1.0', '1', '{}', '/tmp', 0, 1,
                     '{\"kind\":\"local\",\"path\":\"/tmp\"}', NULL)",
        )
        .execute(&pool)
        .await
        .expect("seed pkg_installed");
        pool
    }

    #[tokio::test]
    async fn evaluate_returns_auto_trusted_for_ikenga_builtin() {
        let pool = open_test_pool().await;
        let pkg = pkg_with_perms("com.ikenga.iyke", Permissions::default());
        let state = evaluate(&pool, &pkg, &InstallSource::Builtin, &PathBuf::from("/tmp"))
            .await
            .expect("evaluate");
        assert!(matches!(state, TrustState::AutoTrusted));
        assert!(state.is_allowed());
    }

    #[tokio::test]
    async fn evaluate_returns_auto_granted_for_skill_pack() {
        let pool = open_test_pool().await;
        let pkg = pkg_with_perms("com.evil.studio", Permissions::default());
        let state = evaluate(
            &pool,
            &pkg,
            &InstallSource::Local {
                path: "/tmp".into(),
            },
            &PathBuf::from("/tmp"),
        )
        .await
        .expect("evaluate");
        assert!(matches!(state, TrustState::AutoGranted));
        assert!(state.is_allowed());
    }

    #[tokio::test]
    async fn evaluate_returns_needs_approval_never_for_new_sensitive_pkg() {
        let pool = open_test_pool().await;
        let mut perms = Permissions::default();
        perms.shell_execute.push("bin/run".into());
        let pkg = pkg_with_perms("com.evil.studio", perms);
        let state = evaluate(
            &pool,
            &pkg,
            &InstallSource::Local {
                path: "/tmp".into(),
            },
            &PathBuf::from("/tmp"),
        )
        .await
        .expect("evaluate");
        match state {
            TrustState::NeedsApproval {
                reason: NeedsApprovalReason::Never,
                ..
            } => {}
            other => panic!("expected NeedsApproval/Never, got {other:?}"),
        }
        let s = evaluate(
            &pool,
            &pkg,
            &InstallSource::Local {
                path: "/tmp".into(),
            },
            &PathBuf::from("/tmp"),
        )
        .await
        .expect("evaluate");
        assert!(!s.is_allowed());
    }

    #[tokio::test]
    async fn grant_then_evaluate_returns_granted_then_revoke_flips() {
        let pool = open_test_pool().await;
        let mut perms = Permissions::default();
        perms.shell_execute.push("bin/run".into());
        let pkg = pkg_with_perms("com.evil.studio", perms);

        let summary = summarize_sensitive(&pkg.manifest.permissions);
        grant(&pool, &pkg.manifest.id, &pkg.manifest.version, &summary)
            .await
            .expect("grant");

        let state = evaluate(
            &pool,
            &pkg,
            &InstallSource::Local {
                path: "/tmp".into(),
            },
            &PathBuf::from("/tmp"),
        )
        .await
        .expect("evaluate");
        match state {
            TrustState::Granted { ref version, .. } => assert_eq!(version, "0.1.0"),
            other => panic!("expected Granted, got {other:?}"),
        }
        assert!(state.is_allowed());

        revoke(&pool, &pkg.manifest.id).await.expect("revoke");
        let state = evaluate(
            &pool,
            &pkg,
            &InstallSource::Local {
                path: "/tmp".into(),
            },
            &PathBuf::from("/tmp"),
        )
        .await
        .expect("evaluate");
        match state {
            TrustState::NeedsApproval {
                reason: NeedsApprovalReason::Revoked,
                ..
            } => {}
            other => panic!("expected NeedsApproval/Revoked, got {other:?}"),
        }
        assert!(!state.is_allowed());
    }

    #[tokio::test]
    async fn grant_then_perms_change_invalidates_via_hash_mismatch() {
        let pool = open_test_pool().await;
        let mut perms = Permissions::default();
        perms.shell_execute.push("bin/run".into());
        let pkg_v1 = pkg_with_perms("com.evil.studio", perms);
        let summary = summarize_sensitive(&pkg_v1.manifest.permissions);
        grant(
            &pool,
            &pkg_v1.manifest.id,
            &pkg_v1.manifest.version,
            &summary,
        )
        .await
        .expect("grant v1");

        // v2 widens shell.execute — should re-prompt.
        let mut perms2 = Permissions::default();
        perms2.shell_execute.push("bin/run".into());
        perms2.shell_execute.push("scripts/sh".into());
        let mut pkg_v2 = pkg_with_perms("com.evil.studio", perms2);
        pkg_v2.manifest.version = "0.2.0".into();

        let state = evaluate(
            &pool,
            &pkg_v2,
            &InstallSource::Local {
                path: "/tmp".into(),
            },
            &PathBuf::from("/tmp"),
        )
        .await
        .expect("evaluate v2");
        match state {
            TrustState::NeedsApproval {
                reason:
                    NeedsApprovalReason::PermissionsChanged {
                        ref prior_version, ..
                    },
                ref current_version,
            } => {
                assert_eq!(prior_version, "0.1.0");
                assert_eq!(current_version, "0.2.0");
            }
            other => panic!("expected PermissionsChanged, got {other:?}"),
        }
    }

    // ── WP-04: per-folder Studio project-access grant storage ────────────────
    //
    // These exercise `has_studio_project_grant` / `record_studio_project_grant`
    // directly (the storage seam the `pkg_studio_request_project_access` command
    // is a thin wrapper over). The native trust prompt itself can't run
    // headlessly, so the command's grant/decline branches are asserted here at
    // the storage level: grant ⇒ `record_*` called (row written); decline ⇒
    // `record_*` NOT called (no row). Path canonicalization is covered by the
    // inline tests in `commands/pkg_studio.rs`.

    /// Helper: count granted `__studio_project` rows for a given path.
    async fn studio_grant_rows(pool: &SqlitePool, path: &str) -> i64 {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pkg_permissions_granted
              WHERE pkg_id = ? AND scope_kind = ? AND scope_value = ? AND trust_state = 'granted'",
        )
        .bind(STUDIO_PKG_ID)
        .bind(STUDIO_PROJECT_SCOPE_KIND)
        .bind(path)
        .fetch_one(pool)
        .await
        .expect("count studio grant rows");
        n
    }

    /// insert-once: a single `record_studio_project_grant` writes exactly one
    /// row, and calling it again for the same path stays at exactly one row
    /// (INSERT OR REPLACE on the composite PK — idempotent, no duplicates).
    #[tokio::test]
    async fn studio_grant_inserts_once_and_is_idempotent() {
        let pool = open_test_pool().await;
        let path = "/home/me/Projects/Album A";

        assert_eq!(studio_grant_rows(&pool, path).await, 0, "no row before grant");
        record_studio_project_grant(&pool, path).await.expect("grant");
        assert_eq!(studio_grant_rows(&pool, path).await, 1, "one row after grant");

        // Re-grant the same folder — idempotent, still exactly one row.
        record_studio_project_grant(&pool, path).await.expect("re-grant");
        assert_eq!(
            studio_grant_rows(&pool, path).await,
            1,
            "re-granting the same path must not duplicate the row"
        );
    }

    /// skip-on-rehit: after a grant, `has_studio_project_grant` returns true for
    /// that exact canonical path — this is the constant-time short-circuit the
    /// command uses to skip the prompt on re-open. A *different* path is
    /// unaffected (grants are per-folder, not global).
    #[tokio::test]
    async fn studio_has_grant_true_after_grant_and_scoped_per_path() {
        let pool = open_test_pool().await;
        let granted = "/home/me/Projects/Album A";
        let other = "/home/me/Projects/Album B";

        assert!(
            !has_studio_project_grant(&pool, granted).await.expect("check"),
            "no grant before record"
        );
        record_studio_project_grant(&pool, granted).await.expect("grant");
        assert!(
            has_studio_project_grant(&pool, granted).await.expect("check"),
            "re-hit on the granted folder must short-circuit (has_grant == true)"
        );
        assert!(
            !has_studio_project_grant(&pool, other).await.expect("check"),
            "a different folder must NOT inherit the grant"
        );
    }

    /// decline-no-row: the decline branch of the command never calls
    /// `record_studio_project_grant`, so `has_studio_project_grant` stays false
    /// and no row exists. Modeled here as "check without a preceding record".
    #[tokio::test]
    async fn studio_decline_leaves_no_row() {
        let pool = open_test_pool().await;
        let path = "/home/me/Projects/Declined";

        // Decline == record_* was never called.
        assert!(
            !has_studio_project_grant(&pool, path).await.expect("check"),
            "declined folder must report no grant"
        );
        assert_eq!(
            studio_grant_rows(&pool, path).await,
            0,
            "declined folder must leave zero rows"
        );
    }

    #[tokio::test]
    async fn version_bump_alone_keeps_grant_when_perms_unchanged() {
        let pool = open_test_pool().await;
        let mut perms = Permissions::default();
        perms.shell_execute.push("bin/run".into());
        let pkg_v1 = pkg_with_perms("com.evil.studio", perms.clone());
        let summary = summarize_sensitive(&pkg_v1.manifest.permissions);
        grant(
            &pool,
            &pkg_v1.manifest.id,
            &pkg_v1.manifest.version,
            &summary,
        )
        .await
        .expect("grant v1");

        // v2: same perms set, bumped version — must stay trusted.
        let mut pkg_v2 = pkg_with_perms("com.evil.studio", perms);
        pkg_v2.manifest.version = "0.2.0".into();

        let state = evaluate(
            &pool,
            &pkg_v2,
            &InstallSource::Local {
                path: "/tmp".into(),
            },
            &PathBuf::from("/tmp"),
        )
        .await
        .expect("evaluate v2");
        assert!(matches!(state, TrustState::Granted { .. }));
        assert!(state.is_allowed());
    }
}
