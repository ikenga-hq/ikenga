//! Supabase project config: `<dir>/supabase.json` (URL + anon key, optional
//! service-role key). The desktop passes `app_data_dir`; the daemon passes
//! `--data-dir`. Rationale for a plain JSON manifest rather than the vault is
//! in `commands/supabase_config.rs`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const FILENAME: &str = "supabase.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupabaseConfig {
    pub url: String,
    pub anon_key: String,
    /// Optional: privileged service-role JWT. When present, the FE uses it
    /// as the Authorization Bearer for every Supabase call (bypasses RLS).
    /// Stored here rather than in Stronghold because the snapshot's KDF
    /// (age content) takes seconds-to-minutes per write on this machine,
    /// which deadlocks the FE save flow. Risk: this file is plain JSON, but
    /// it's chmod 0600 below and lives only in app_data_dir — same boundary
    /// as the previous `.env.local`.
    ///
    /// G-PRINCIPAL / WP-20 seam: the daemon returns this field to the remote
    /// client from `supabase_config_get`, because `src/lib/supabase.ts` uses
    /// it as its bearer — desktop parity under today's single-token owner
    /// model, where whoever holds the daemon token already owns this file.
    /// Under multi-user it must become per-principal or be withheld.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_role_key: Option<String>,
}

/// `<dir>/supabase.json`, creating `dir` if needed (as the desktop always has).
pub fn config_path(dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir: {e}"))?;
    Ok(dir.join(FILENAME))
}

/// `None` when the file is absent — the FE's "not configured" signal.
pub fn get(dir: &Path) -> Result<Option<SupabaseConfig>, String> {
    let path = config_path(dir)?;
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read: {e}"))?;
    let cfg: SupabaseConfig = serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))?;
    Ok(Some(cfg))
}

/// `service_role_key`: `None` preserves the stored key, `Some("")` (blank)
/// clears it, anything else replaces it.
pub fn set(
    dir: &Path,
    url: String,
    anon_key: String,
    service_role_key: Option<String>,
) -> Result<(), String> {
    if url.trim().is_empty() || anon_key.trim().is_empty() {
        return Err("url and anon_key are required".into());
    }
    // Preserve the existing service_role_key if the caller didn't pass one,
    // so the URL/anon-key form doesn't accidentally wipe it.
    let merged_service_role = match service_role_key {
        Some(s) if !s.trim().is_empty() => Some(s),
        Some(_) => None, // explicit empty string → clear
        None => {
            // not supplied → preserve existing
            let path = config_path(dir)?;
            if path.exists() {
                std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| serde_json::from_str::<SupabaseConfig>(&t).ok())
                    .and_then(|c| c.service_role_key)
            } else {
                None
            }
        }
    };
    let cfg = SupabaseConfig {
        url,
        anon_key,
        service_role_key: merged_service_role,
    };
    let path = config_path(dir)?;
    let json = serde_json::to_string_pretty(&cfg).map_err(|e| format!("serialize: {e}"))?;
    // Atomic write: temp + rename, so a crash mid-write can't corrupt.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write tmp: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod: {e}"))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

pub fn clear(dir: &Path) -> Result<(), String> {
    let path = config_path(dir)?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("remove: {e}"))?;
    }
    Ok(())
}
