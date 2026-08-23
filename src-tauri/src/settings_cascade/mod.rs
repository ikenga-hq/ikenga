//! 5-tier settings cascade merge engine with provenance tracking (WP-06).
//!
//! Merges settings across:
//!   1. Managed  (`/etc/claude/settings.json` or `/etc/claude.json`)
//!   2. User     (`~/.claude/settings.json`)
//!   3. Project  (`<project_dir>/.claude/settings.json`)
//!   4. Local    (`<project_dir>/.claude/settings.local.json`)
//!   5. Overlay  (`<overlay_dir>/settings.json` when active)
//!
//! Tracks which file/tier won each top-level key in the effective output.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SettingsTier {
    Managed,
    User,
    Project,
    Local,
    Overlay,
}

impl SettingsTier {
    pub fn precedence(self) -> u8 {
        match self {
            SettingsTier::Managed => 0,
            SettingsTier::User => 1,
            SettingsTier::Project => 2,
            SettingsTier::Local => 3,
            SettingsTier::Overlay => 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyProvenance {
    pub key: String,
    pub winning_tier: SettingsTier,
    pub source_file: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CascadeResult {
    pub merged: serde_json::Value,
    pub provenance: Vec<KeyProvenance>,
    pub files_read: Vec<String>,
}

fn read_json_file(path: &Path) -> Option<serde_json::Value> {
    if !path.is_file() {
        return None;
    }
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn resolve_settings_cascade(
    project_dir: Option<&Path>,
    overlay_dir: Option<&Path>,
) -> CascadeResult {
    let mut files_read = Vec::new();
    let mut tier_files: Vec<(SettingsTier, PathBuf)> = Vec::new();

    // 1. Managed tier
    let managed_path = PathBuf::from("/etc/claude/settings.json");
    if managed_path.is_file() {
        tier_files.push((SettingsTier::Managed, managed_path));
    }

    // 2. User tier
    let expanded_user = shellexpand::tilde("~/.claude/settings.json").to_string();
    let user_path = PathBuf::from(expanded_user);
    if user_path.is_file() {
        tier_files.push((SettingsTier::User, user_path));
    }

    // 3. Project & 4. Local tiers
    if let Some(pdir) = project_dir {
        let proj_path = pdir.join(".claude").join("settings.json");
        if proj_path.is_file() {
            tier_files.push((SettingsTier::Project, proj_path));
        }
        let local_path = pdir.join(".claude").join("settings.local.json");
        if local_path.is_file() {
            tier_files.push((SettingsTier::Local, local_path));
        }
    }

    // 5. Overlay tier
    if let Some(odir) = overlay_dir {
        let overlay_path = odir.join("settings.json");
        if overlay_path.is_file() {
            tier_files.push((SettingsTier::Overlay, overlay_path));
        }
    }

    let mut merged_obj = serde_json::Map::new();
    let mut provenance_map: BTreeMap<String, KeyProvenance> = BTreeMap::new();

    for (tier, path) in tier_files {
        let file_str = path.to_string_lossy().to_string();
        files_read.push(file_str.clone());

        if let Some(val) = read_json_file(&path) {
            if let Some(obj) = val.as_object() {
                for (key, value) in obj {
                    // Deep merge objects or replace primitives
                    if let Some(existing) = merged_obj.get_mut(key) {
                        if existing.is_object() && value.is_object() {
                            if let (Some(ex_obj), Some(new_obj)) =
                                (existing.as_object_mut(), value.as_object())
                            {
                                for (k2, v2) in new_obj {
                                    ex_obj.insert(k2.clone(), v2.clone());
                                }
                            }
                        } else {
                            merged_obj.insert(key.clone(), value.clone());
                        }
                    } else {
                        merged_obj.insert(key.clone(), value.clone());
                    }

                    provenance_map.insert(
                        key.clone(),
                        KeyProvenance {
                            key: key.clone(),
                            winning_tier: tier,
                            source_file: file_str.clone(),
                            value: value.clone(),
                        },
                    );
                }
            }
        }
    }

    CascadeResult {
        merged: serde_json::Value::Object(merged_obj),
        provenance: provenance_map.into_values().collect(),
        files_read,
    }
}

/// Tauri command to resolve effective settings cascade for a project directory.
#[tauri::command]
pub fn claude_config_resolve_cascade(
    project_dir: Option<String>,
    overlay_dir: Option<String>,
) -> CascadeResult {
    let p_path = project_dir.as_ref().map(Path::new);
    let o_path = overlay_dir.as_ref().map(Path::new);
    resolve_settings_cascade(p_path, o_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_cascade_merge_provenance() {
        let tmp = TempDir::new().expect("tempdir");
        let proj_dir = tmp.path().join(".claude");
        fs::create_dir_all(&proj_dir).expect("create proj .claude");

        let proj_settings = proj_dir.join("settings.json");
        fs::write(
            &proj_settings,
            r#"{"permissionMode":"plan","statusLine":{"refreshInterval":500}}"#,
        )
        .expect("write proj settings");

        let local_settings = proj_dir.join("settings.local.json");
        fs::write(
            &local_settings,
            r#"{"permissionMode":"default"}"#,
        )
        .expect("write local settings");

        let cascade = resolve_settings_cascade(Some(tmp.path()), None);
        assert_eq!(cascade.merged["permissionMode"], "default");

        let prov_perm = cascade
            .provenance
            .iter()
            .find(|p| p.key == "permissionMode")
            .expect("provenance entry");
        assert_eq!(prov_perm.winning_tier, SettingsTier::Local);
    }
}
