//! Settings-TOML merge engine (WP-22, v2b) — the TOML sibling of
//! `merge.rs`'s JSON path, kept **deliberately disjoint** from it.
//!
//! Splices a single keyed block into / out of Codex's TOML config files while
//! preserving every unrelated key — including whitespace, key order, and
//! comments — by editing a `toml_edit::Document` in place rather than
//! round-tripping through a value model. Plain `toml::Value` re-serialization
//! reorders keys and drops comments, which would break the "unrelated keys
//! byte-identical" guarantee; `Document` does not.
//!
//! Two block families (both inside `~/.codex/config.toml` per the frozen
//! `EngineLayout`):
//!   - **`[mcp_servers.<name>]`** — a Codex MCP server definition (lenient
//!     top-level table).
//!   - **`[hooks]`** — inline hook tables keyed by event name.
//!
//! Mechanic (every mutation): **read whole document → parse as
//! `Document` → splice the single child key under the fixed parent table →
//! serialize → temp-file in the same dir → atomic rename.** Untouched keys are
//! byte-preserved because `toml_edit` retains their original spans.
//!
//! Pure functions only — no Tauri commands, no shared state. Path resolution
//! and scope handling live in the caller (`merge.rs` dispatch + `claude_store`
//! command layer). This module never resolves `~` or a project root itself;
//! it operates on a concrete `&Path` it is handed.
//!
//! Codex `config.toml` is **lenient** (`strict_keys = false` in the layout),
//! so this module performs no strict-key guard — that guard is JSON/Gemini-only
//! and lives in `merge.rs`.

// The whole TOML merge surface is the frozen G-WRITE contract consumed by the
// downstream write WPs (WP-23 file writes, WP-24 cross-engine transcode). It is
// exercised by this module's tests but not yet called from a registered Tauri
// command, so the lib build reports it unused — expected for a gate WP that
// publishes an interface ahead of its callers.
#![allow(dead_code)]

use std::path::Path;

use toml_edit::{Document, Item, Table, Value};

use super::merge::StoreError;

/// A serde-serializable block to splice. The caller passes a `serde_json::Value`
/// (the same shape the JSON path uses for `server_def` / hook blocks); we
/// transcode it into a `toml_edit::Item` so the TOML and JSON write paths share
/// one wire vocabulary upstream.
pub type Block = serde_json::Value;

/// Enable (insert or overwrite) `mcp_servers.<name>` in a Codex `config.toml`.
/// `server_def` is the table placed at that key. All other keys in the file —
/// `model`, `[mcp_servers.<other>]`, comments, ordering — are preserved
/// byte-for-byte.
pub fn enable_mcp(config_toml: &Path, name: &str, server_def: Block) -> Result<(), StoreError> {
    guard_toml(config_toml, "mcp_servers", name, &server_def, false)?;
    splice(config_toml, "mcp_servers", name, Some(server_def))
}

/// Disable (remove) `mcp_servers.<name>` from a Codex `config.toml`. Removing
/// the last server leaves an empty `[mcp_servers]` table; every other key is
/// preserved. A missing file / parent / child is a no-op (no write, no error).
/// Removed only if it is exactly the store's `store_def`; a user's own or an
/// edited server is refused (see `merge.rs`'s ownership guard note).
pub fn disable_mcp(config_toml: &Path, name: &str, store_def: &Block) -> Result<(), StoreError> {
    guard_toml(config_toml, "mcp_servers", name, store_def, true)?;
    splice(config_toml, "mcp_servers", name, None)
}

/// Enable (insert or overwrite) an inline hook block keyed by `event` under the
/// top-level `[hooks]` table in a Codex `config.toml`. `block` is the value
/// placed at `hooks.<event>`. All other keys preserved.
pub fn enable_hook(config_toml: &Path, event: &str, block: Block) -> Result<(), StoreError> {
    guard_toml(config_toml, "hooks", event, &block, false)?;
    splice(config_toml, "hooks", event, Some(block))
}

/// Disable (remove) the inline hook block keyed by `event` from `[hooks]` in a
/// Codex `config.toml`. Missing file / key is a no-op.
/// Removed only if it is exactly the store's `store_block`.
pub fn disable_hook(config_toml: &Path, event: &str, store_block: &Block) -> Result<(), StoreError> {
    guard_toml(config_toml, "hooks", event, store_block, true)?;
    splice(config_toml, "hooks", event, None)
}

// ─── Ownership guard (TOML) ───────────────────────────────────────────────────
//
// Same rule as `merge.rs`: only replace or remove an entry that is exactly what
// the store puts there. The comparison is semantic, not textual, so key order
// and comments don't matter: the live item is read back as JSON, and the store
// block is passed through the *same* JSON→TOML conversion `enable_*` uses before
// being read back, so an untouched entry always compares equal to its source.

/// A TOML item as a JSON value, for comparison.
fn item_to_json(item: &Item) -> Option<serde_json::Value> {
    match item {
        Item::None => None,
        Item::Value(v) => Some(value_to_json(v)),
        Item::Table(t) => Some(serde_json::Value::Object(
            t.iter()
                .filter_map(|(k, v)| item_to_json(v).map(|j| (k.to_string(), j)))
                .collect(),
        )),
        Item::ArrayOfTables(a) => Some(serde_json::Value::Array(
            a.iter()
                .filter_map(|t| item_to_json(&Item::Table(t.clone())))
                .collect(),
        )),
    }
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::String(s) => serde_json::Value::String(s.value().clone()),
        Value::Integer(i) => serde_json::Value::from(*i.value()),
        Value::Float(f) => serde_json::Number::from_f64(*f.value())
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Boolean(b) => serde_json::Value::Bool(*b.value()),
        Value::Datetime(d) => serde_json::Value::String(d.value().to_string()),
        Value::Array(a) => serde_json::Value::Array(a.iter().map(value_to_json).collect()),
        Value::InlineTable(t) => serde_json::Value::Object(
            t.iter().map(|(k, v)| (k.to_string(), value_to_json(v))).collect(),
        ),
    }
}

fn guard_toml(
    path: &Path,
    parent_key: &str,
    child_key: &str,
    store: &Block,
    removing: bool,
) -> Result<(), StoreError> {
    if !path.exists() {
        return Ok(());
    }
    let doc = read_document(path)?;
    let current = doc
        .get(parent_key)
        .and_then(Item::as_table_like)
        .and_then(|t| t.get(child_key))
        .and_then(item_to_json);
    let Some(current) = current else {
        return Ok(());
    };
    // A store block with no TOML representation can't match anything on disk.
    let expected = json_to_toml_item(store.clone(), path)
        .ok()
        .and_then(|i| item_to_json(&i));
    if expected.as_ref() == Some(&current) {
        return Ok(());
    }
    Err(StoreError::NotOurs {
        path: path.to_string_lossy().to_string(),
        key: format!("{parent_key}.{child_key}"),
        reason: super::merge::ownership_conflict_reason(removing),
    })
}

// ─── Core splice ──────────────────────────────────────────────────────────────

/// Read `path` as a TOML document, set or remove `parent_key.child_key`, then
/// atomically write it back. `value = Some(_)` inserts/overwrites; `None`
/// removes. All sibling keys (and formatting) are carried through untouched.
///
/// Removal of a missing file / parent / child is a no-op (no write, no error) —
/// so a `disable_*` on a clean machine never materializes a file.
fn splice(
    path: &Path,
    parent_key: &str,
    child_key: &str,
    value: Option<Block>,
) -> Result<(), StoreError> {
    let removing = value.is_none();
    let existed = path.exists();

    // No-op fast path for removal so we never create a file just to delete from
    // it.
    if removing && !existed {
        return Ok(());
    }

    let mut doc = read_document(path)?;

    if removing {
        // Only descend if the parent is a table holding the child.
        let (removed, parent_now_empty) = match doc.get_mut(parent_key) {
            Some(item) => match item.as_table_like_mut() {
                Some(tbl) => {
                    let removed = tbl.remove(child_key).is_some();
                    (removed, tbl.is_empty())
                }
                None => (false, false),
            },
            None => (false, false),
        };
        if !removed {
            // Nothing to do — leave the file byte-identical by not rewriting.
            return Ok(());
        }
        // If we just removed the last child, drop the now-empty parent table so
        // an enable→disable round-trip returns the file byte-identical (an
        // `enable` that creates the parent via dotted/implicit syntax must not
        // leave a bare `[parent]` header behind). Sibling keys are untouched.
        if parent_now_empty {
            doc.remove(parent_key);
        }
        write_document(path, &doc)?;
        return Ok(());
    }

    // Insert / overwrite. Get-or-create the parent table; refuse to clobber a
    // non-table parent (a user-set scalar there is load-bearing).
    let block = value.expect("checked Some above");
    let block_item = json_to_toml_item(block, path)?;

    let entry = doc
        .entry(parent_key)
        .or_insert_with(|| Item::Table(Table::new()));
    match entry.as_table_like_mut() {
        Some(tbl) => {
            tbl.insert(child_key, block_item);
        }
        None => {
            return Err(StoreError::NonTableParent {
                path: path.to_string_lossy().to_string(),
                key: parent_key.to_string(),
            });
        }
    }
    write_document(path, &doc)?;
    Ok(())
}

/// Read a TOML file into a `Document`. Missing / empty file → empty document
/// so first-write works on a clean machine. A parse error is a typed error.
fn read_document(path: &Path) -> Result<Document, StoreError> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(Document::new()),
        Ok(s) => s.parse::<Document>().map_err(|e| StoreError::Parse {
            path: path.to_string_lossy().to_string(),
            message: e.to_string(),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Document::new()),
        Err(e) => Err(StoreError::Io {
            path: path.to_string_lossy().to_string(),
            message: e.to_string(),
        }),
    }
}

/// Atomic write: serialize the document → temp file in the same dir → rename.
/// `toml_edit`'s `to_string()` reproduces the preserved formatting verbatim, so
/// a round-trip enable→disable that removes only the added key returns the file
/// to byte-identical state.
fn write_document(path: &Path, doc: &Document) -> Result<(), StoreError> {
    let parent = path.parent().ok_or_else(|| StoreError::Io {
        path: path.to_string_lossy().to_string(),
        message: "config path has no parent".to_string(),
    })?;
    std::fs::create_dir_all(parent).map_err(|e| StoreError::Io {
        path: parent.to_string_lossy().to_string(),
        message: format!("mkdir: {e}"),
    })?;
    let rendered = doc.to_string();
    let tmp_name = format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config.toml".to_string()),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let tmp = parent.join(tmp_name);
    std::fs::write(&tmp, rendered).map_err(|e| StoreError::Io {
        path: tmp.to_string_lossy().to_string(),
        message: format!("write: {e}"),
    })?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        StoreError::Io {
            path: path.to_string_lossy().to_string(),
            message: format!("rename {}: {e}", tmp.display()),
        }
    })?;
    Ok(())
}

// ─── JSON → TOML transcode (for the spliced block only) ───────────────────────

/// Transcode a `serde_json::Value` into a `toml_edit::Item`. The caller's block
/// is JSON-shaped (shared wire with the JSON merge path); TOML cannot represent
/// `null`, so a `null` anywhere in the block is a typed error rather than a
/// silent drop.
///
/// A top-level **object** block becomes a real `Item::Table` (so it renders as a
/// `[parent.child]` header rather than promoting an explicit `[parent]` header
/// with an inline child — which would perturb an existing implicit-table
/// parent's formatting and break byte-identity). Nested objects inside the
/// block stay inline tables. Arrays / scalars become values.
fn json_to_toml_item(v: serde_json::Value, path: &Path) -> Result<Item, StoreError> {
    match v {
        serde_json::Value::Object(map) => {
            let mut tbl = Table::new();
            for (k, val) in map {
                tbl.insert(&k, Item::Value(json_to_toml_value(val, path)?));
            }
            Ok(Item::Table(tbl))
        }
        other => Ok(Item::Value(json_to_toml_value(other, path)?)),
    }
}

fn json_to_toml_value(v: serde_json::Value, path: &Path) -> Result<Value, StoreError> {
    use serde_json::Value as J;
    match v {
        J::Null => Err(StoreError::UnrepresentableValue {
            path: path.to_string_lossy().to_string(),
            message: "TOML cannot represent a null value".to_string(),
        }),
        J::Bool(b) => Ok(Value::from(b)),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::from(i))
            } else if let Some(f) = n.as_f64() {
                Ok(Value::from(f))
            } else {
                Err(StoreError::UnrepresentableValue {
                    path: path.to_string_lossy().to_string(),
                    message: format!("number {n} is not representable in TOML"),
                })
            }
        }
        J::String(s) => Ok(Value::from(s)),
        J::Array(items) => {
            let mut arr = toml_edit::Array::new();
            for it in items {
                arr.push(json_to_toml_value(it, path)?);
            }
            Ok(Value::Array(arr))
        }
        J::Object(map) => {
            let mut tbl = toml_edit::InlineTable::new();
            for (k, val) in map {
                tbl.insert(&k, json_to_toml_value(val, path)?);
            }
            Ok(Value::InlineTable(tbl))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    // ── Ownership guard (TOML) ──────────────────────────────────────────────

    #[test]
    fn toml_disable_mcp_refuses_a_users_own_server() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("config.toml");
        fs::write(&p, "[mcp_servers.github]\ncommand = \"gh-mcp\"\n").unwrap();
        let before = fs::read(&p).unwrap();

        let err = disable_mcp(&p, "github", &json!({ "command": "other" })).unwrap_err();
        assert!(matches!(err, StoreError::NotOurs { .. }), "{err:?}");
        assert_eq!(fs::read(&p).unwrap(), before);
    }

    /// The comparison is semantic: the store's own entry is recognised even if
    /// its keys were reordered and a comment added, so it can still be disabled.
    #[test]
    fn toml_recognises_its_own_entry_despite_formatting() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("config.toml");
        fs::write(
            &p,
            "# mine\n[mcp_servers.royalti]\n# reordered by hand\nargs = [\"--stdio\"]\ncommand = \"royalti-mcp\"\n",
        )
        .unwrap();
        let def = json!({ "command": "royalti-mcp", "args": ["--stdio"] });
        disable_mcp(&p, "royalti", &def).unwrap();
        assert!(!fs::read_to_string(&p).unwrap().contains("royalti-mcp"));
    }

    #[test]
    fn toml_enable_hook_refuses_to_replace_an_existing_event() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("config.toml");
        fs::write(&p, "[hooks]\nPreToolUse = { type = \"command\", command = \"mine\" }\n").unwrap();
        let before = fs::read(&p).unwrap();

        let err = enable_hook(&p, "PreToolUse", json!({ "type": "command", "command": "store" }))
            .unwrap_err();
        assert!(matches!(err, StoreError::NotOurs { .. }), "{err:?}");
        assert_eq!(fs::read(&p).unwrap(), before);
    }

    /// Seed a Codex `config.toml` with several unrelated keys + comments we must
    /// never disturb.
    fn seed_config(dir: &Path) -> std::path::PathBuf {
        let p = dir.join("config.toml");
        fs::write(
            &p,
            // Deliberately non-canonical formatting: a comment, blank lines, an
            // existing mcp server. `toml_edit` must preserve all of it.
            "# Codex config — do not reformat\nmodel = \"o3\"\n\n[mcp_servers.exa]\ncommand = \"exa-mcp\"\nargs = []\n",
        )
        .unwrap();
        p
    }

    #[test]
    fn mcp_round_trip_byte_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let p = seed_config(tmp.path());
        let baseline = fs::read(&p).unwrap();

        let def = json!({ "command": "royalti-mcp", "args": ["--stdio"] });
        enable_mcp(&p, "royalti", def.clone()).unwrap();

        // The new server landed; exa + model + the comment are still there.
        let after_add = fs::read_to_string(&p).unwrap();
        assert!(after_add.contains("[mcp_servers.royalti]") || after_add.contains("royalti ="));
        assert!(after_add.contains("# Codex config — do not reformat"));
        assert!(after_add.contains("exa-mcp"));
        let parsed: toml::Value = toml::from_str(&after_add).unwrap();
        assert_eq!(
            parsed
                .get("mcp_servers")
                .and_then(|m| m.get("royalti"))
                .and_then(|r| r.get("command"))
                .and_then(|c| c.as_str()),
            Some("royalti-mcp")
        );

        disable_mcp(&p, "royalti", &def).unwrap();
        let after_remove = fs::read(&p).unwrap();
        assert_eq!(
            after_remove, baseline,
            "config.toml byte-identical after enable→disable (comment/order/exa preserved)"
        );
    }

    #[test]
    fn hook_round_trip_byte_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let p = seed_config(tmp.path());
        let baseline = fs::read(&p).unwrap();

        let block = json!({ "type": "command", "command": "echo hi" });
        enable_hook(&p, "PreToolUse", block.clone()).unwrap();
        let after_add = fs::read_to_string(&p).unwrap();
        assert!(after_add.contains("PreToolUse"));
        assert!(after_add.contains("# Codex config — do not reformat"));

        disable_hook(&p, "PreToolUse", &block).unwrap();
        let after_remove = fs::read(&p).unwrap();
        assert_eq!(
            after_remove, baseline,
            "config.toml byte-identical after hook enable→disable"
        );
    }

    #[test]
    fn disable_on_clean_machine_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("config.toml");
        disable_mcp(&p, "nope", &json!({})).unwrap();
        disable_hook(&p, "Stop", &json!({})).unwrap();
        assert!(!p.exists(), "no file materialized by a no-op disable");
    }

    #[test]
    fn enable_on_clean_machine_creates_minimal_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("config.toml");
        enable_mcp(&p, "royalti", json!({ "command": "royalti-mcp" })).unwrap();
        let parsed: toml::Value = toml::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(
            parsed.pointer_command("mcp_servers", "royalti"),
            Some("royalti-mcp")
        );
    }

    #[test]
    fn refuses_non_table_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("config.toml");
        // mcp_servers is a scalar — must refuse rather than clobber.
        fs::write(&p, "mcp_servers = 3\n").unwrap();
        let err = enable_mcp(&p, "x", json!({ "command": "c" })).unwrap_err();
        assert!(
            matches!(err, StoreError::NonTableParent { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn null_block_is_typed_error_not_silent_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("config.toml");
        let err = enable_mcp(&p, "x", json!({ "command": null })).unwrap_err();
        assert!(
            matches!(err, StoreError::UnrepresentableValue { .. }),
            "got {err:?}"
        );
        // Nothing written.
        assert!(!p.exists());
    }

    // small helper for the assertion above
    trait PointerCommand {
        fn pointer_command(&self, a: &str, b: &str) -> Option<&str>;
    }
    impl PointerCommand for toml::Value {
        fn pointer_command(&self, a: &str, b: &str) -> Option<&str> {
            self.get(a)
                .and_then(|v| v.get(b))
                .and_then(|v| v.get("command"))
                .and_then(|v| v.as_str())
        }
    }
}
