//! WP-02 — Ọba registry index I/O (`store/registry.json`).
//!
//! The single JSON sidecar that turns the copy-vault into an INDEX. This module
//! owns reading, writing, and back-filling it; the shape (`RegistryFile` /
//! `ClaudeStoreEntry` / `RegistryProvenance`) is frozen in the parent module by
//! `G-SCHEMA` (WP-01).
//!
//! Design contract (`plans/oba-registry/drafts/registry-schema.md`):
//! - **Provenance is stored here; dependents are NOT.** Dependents are computed
//!   live by the scanner (WP-04). So a missing or corrupt index is *non-fatal* —
//!   `load` degrades to an empty index and the safe-delete guard still works off
//!   the live filesystem.
//! - **Back-compat**: an entry without provenance (or with an empty
//!   `canonicalPath`) normalizes to a synthesized `local`, shell-managed entry
//!   whose canonical is its store path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{ClaudeStoreEntry, RegistryFile, RegistryProvenance};

/// Path of the index sidecar inside a given store root.
pub fn registry_path(store: &Path) -> PathBuf {
    store.join("registry.json")
}

/// Load `store/registry.json`. **Never fails**: a missing file is an empty
/// index; a corrupt/unparseable file is logged and treated as empty so a bad
/// write can never wedge the store (the live scanner still protects deletes).
/// Each entry is normalized so an empty `canonicalPath` resolves to its
/// `storePath` (back-compat for pre-registry data).
pub fn load(store: &Path) -> RegistryFile {
    let path = registry_path(store);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return RegistryFile::default(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "registry.json unreadable; using empty index");
            return RegistryFile::default();
        }
    };
    match serde_json::from_str::<RegistryFile>(&raw) {
        Ok(mut rf) => {
            for e in &mut rf.entries {
                normalize_entry(e);
            }
            rf
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "registry.json corrupt; using empty index");
            RegistryFile::default()
        }
    }
}

/// Atomically write the index (temp file in the same dir + rename, so a crash
/// mid-write never leaves a partial `registry.json`). Creates the store dir if
/// absent.
pub fn save(store: &Path, rf: &RegistryFile) -> Result<(), String> {
    std::fs::create_dir_all(store).map_err(|e| format!("create store dir: {e}"))?;
    let path = registry_path(store);
    let json = serde_json::to_string_pretty(rf).map_err(|e| format!("serialize registry: {e}"))?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = store.join(format!(".registry.json.tmp.{nonce}"));
    std::fs::write(&tmp, json.as_bytes()).map_err(|e| format!("write temp registry: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename registry into place: {e}")
    })?;
    Ok(())
}

/// Normalize a single entry's provenance: an empty `canonicalPath` (legacy data)
/// resolves to the entry's `storePath`.
fn normalize_entry(e: &mut ClaudeStoreEntry) {
    if e.provenance.canonical_path.is_empty() {
        e.provenance.canonical_path = e.store_path.clone();
    }
}

/// Build a `(kind, name) → provenance` lookup from the index, for overlaying
/// stored provenance onto freshly-scanned store entries in `claude_store_list`.
pub fn provenance_map(rf: &RegistryFile) -> HashMap<(String, String), RegistryProvenance> {
    rf.entries
        .iter()
        .map(|e| ((e.kind.clone(), e.name.clone()), e.provenance.clone()))
        .collect()
}

/// Overlay stored provenance onto a scanned entry where the index has a record
/// for `(kind, name)`. Entries with no record keep their synthesized-`local`
/// provenance (set at scan time), so an absent index = every entry reads local.
pub fn overlay_provenance(
    entry: &mut ClaudeStoreEntry,
    map: &HashMap<(String, String), RegistryProvenance>,
) {
    if let Some(p) = map.get(&(entry.kind.clone(), entry.name.clone())) {
        entry.provenance = p.clone();
    }
}

/// Upsert an EXTERNAL master record (`managed:false`, kept-in-place) into the
/// index. Used by the back-fill path (`oba_backfill_registry`) so the
/// safe-delete guard can resolve a real external canonical (e.g. `groundwork`
/// published from `ikenga-pkgs`) instead of a nonexistent vault path. Sets
/// `canonicalPath` to the real master and `managed:false`; **never stores
/// dependents** (live-computed per the frozen `G-SCHEMA` contract). Returns
/// `true` iff the index changed (new record, or an existing record's
/// canonical/managed differed).
pub fn upsert_external(
    rf: &mut RegistryFile,
    kind: &str,
    name: &str,
    canonical_path: &str,
) -> bool {
    if let Some(e) = rf
        .entries
        .iter_mut()
        .find(|e| e.kind == kind && e.name == name)
    {
        if e.provenance.canonical_path == canonical_path && !e.provenance.managed {
            return false; // already an external master at this path
        }
        e.provenance.canonical_path = canonical_path.to_string();
        e.provenance.managed = false;
        return true;
    }
    let mut prov = RegistryProvenance::local(canonical_path.to_string());
    prov.managed = false;
    rf.entries.push(ClaudeStoreEntry {
        kind: kind.to_string(),
        name: name.to_string(),
        store_path: canonical_path.to_string(),
        description: None,
        modified_ms: 0,
        enabled_in: Vec::new(),
        // An in-place external master carries no declared forward deps here;
        // requires (if any) arrive via the compiled manifest/registry record.
        requires: Vec::new(),
        // An external master is a leaf primitive; bundle members are recorded
        // only by the bundle installer (WP-19).
        members: Vec::new(),
        provenance: prov,
    });
    true
}

/// Upsert a full managed record (Phase 2 install / update). Replaces an existing
/// `(kind, name)` entry wholesale or appends a new one. Unlike [`upsert_external`]
/// (which only flips an external master in place), this carries the complete
/// `RegistryProvenance` a git/npx install resolves — `source`, `url`, `ref`,
/// resolved `version`, `managed:true`, and timestamps. Callers persist with [`save`].
pub fn upsert_record(rf: &mut RegistryFile, entry: ClaudeStoreEntry) {
    if let Some(e) = rf
        .entries
        .iter_mut()
        .find(|e| e.kind == entry.kind && e.name == entry.name)
    {
        *e = entry;
    } else {
        rf.entries.push(entry);
    }
}

/// Drop the record for `(kind, name)` from the index. Returns `true` iff a
/// record existed and was removed. Touches no files on disk — provenance only
/// (the master + any symlinks stay exactly as they are). Backs `oba_forget`.
pub fn forget(rf: &mut RegistryFile, kind: &str, name: &str) -> bool {
    let before = rf.entries.len();
    rf.entries.retain(|e| !(e.kind == kind && e.name == name));
    rf.entries.len() != before
}

/// Build an index from the currently-scanned entries, synthesizing `local`
/// provenance for each — the first-build back-fill. Callers persist the result
/// with [`save`]. (Read paths do not auto-write; back-fill is an explicit op.)
#[allow(dead_code)]
pub fn backfill_local(entries: &[ClaudeStoreEntry]) -> RegistryFile {
    let mut rf = RegistryFile::default();
    rf.entries = entries
        .iter()
        .map(|e| {
            let mut e = e.clone();
            e.provenance = RegistryProvenance::local(e.store_path.clone());
            e.enabled_in = Vec::new(); // index records provenance, not live scope state
            e
        })
        .collect();
    rf
}

#[cfg(test)]
mod tests {
    use super::super::ProvenanceSource;
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("ngwa_reg_{tag}_{nonce}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn entry(kind: &str, name: &str, store: &Path, prov: RegistryProvenance) -> ClaudeStoreEntry {
        ClaudeStoreEntry {
            kind: kind.into(),
            name: name.into(),
            store_path: store
                .join(format!("{kind}s/{name}"))
                .to_string_lossy()
                .into(),
            description: None,
            modified_ms: 0,
            enabled_in: vec![],
            requires: vec![],
            members: vec![],
            provenance: prov,
        }
    }

    #[test]
    fn load_missing_is_empty_not_error() {
        let store = tmp("missing");
        let rf = load(&store);
        // WP-11: a missing index yields the current-version default (v2).
        assert_eq!(rf.schema_version, 2);
        assert!(rf.entries.is_empty());
        std::fs::remove_dir_all(&store).ok();
    }

    #[test]
    fn load_corrupt_degrades_to_empty() {
        let store = tmp("corrupt");
        std::fs::write(registry_path(&store), b"{ this is not json").unwrap();
        let rf = load(&store);
        assert!(
            rf.entries.is_empty(),
            "corrupt index must degrade, not panic"
        );
        std::fs::remove_dir_all(&store).ok();
    }

    #[test]
    fn save_then_load_round_trips_atomically() {
        let store = tmp("roundtrip");
        let git = RegistryProvenance {
            source: ProvenanceSource::Git,
            url: Some("github:obra/huashu".into()),
            r#ref: Some("v2.1.0".into()),
            version: Some("abc123".into()),
            canonical_path: store.join("skills/huashu").to_string_lossy().into(),
            managed: false,
            installed_at: None,
            updated_at: None,
            from_catalog: false,
            auto_update: false,
        };
        let mut rf = RegistryFile::default();
        rf.entries
            .push(entry("skill", "huashu", &store, git.clone()));
        save(&store, &rf).unwrap();
        // no leftover temp files
        let leftovers: Vec<_> = std::fs::read_dir(&store)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "atomic save left a temp file");
        let back = load(&store);
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.entries[0].provenance, git);
        std::fs::remove_dir_all(&store).ok();
    }

    #[test]
    fn empty_canonical_normalizes_to_store_path() {
        let store = tmp("normalize");
        // hand-write an entry whose provenance omits canonicalPath (legacy shape)
        let raw = format!(
            r#"{{"schemaVersion":1,"entries":[{{"kind":"skill","name":"x","storePath":"{}","description":null,"modifiedMs":0,"enabledIn":[]}}]}}"#,
            store.join("skills/x").to_string_lossy()
        );
        std::fs::write(registry_path(&store), raw).unwrap();
        let rf = load(&store);
        assert_eq!(
            rf.entries[0].provenance.canonical_path,
            store.join("skills/x").to_string_lossy()
        );
        assert_eq!(rf.entries[0].provenance.source, ProvenanceSource::Local);
        assert!(rf.entries[0].provenance.managed);
        std::fs::remove_dir_all(&store).ok();
    }

    #[test]
    fn overlay_applies_stored_provenance_else_keeps_local() {
        let store = tmp("overlay");
        let git = RegistryProvenance {
            source: ProvenanceSource::Git,
            url: Some("u".into()),
            r#ref: None,
            version: Some("sha".into()),
            canonical_path: "/elsewhere/groundwork".into(),
            managed: false,
            installed_at: None,
            updated_at: None,
            from_catalog: false,
            auto_update: false,
        };
        let mut rf = RegistryFile::default();
        rf.entries
            .push(entry("skill", "groundwork", &store, git.clone()));
        let map = provenance_map(&rf);

        // a scanned entry that IS in the index gets the external provenance
        let mut scanned = entry(
            "skill",
            "groundwork",
            &store,
            RegistryProvenance::local(store.join("skills/groundwork").to_string_lossy().into()),
        );
        overlay_provenance(&mut scanned, &map);
        assert_eq!(scanned.provenance.source, ProvenanceSource::Git);
        assert!(!scanned.provenance.managed);

        // a scanned entry NOT in the index keeps its synthesized local
        let mut other = entry(
            "skill",
            "release-status",
            &store,
            RegistryProvenance::local(store.join("skills/release-status").to_string_lossy().into()),
        );
        overlay_provenance(&mut other, &map);
        assert_eq!(other.provenance.source, ProvenanceSource::Local);
        assert!(other.provenance.managed);
        std::fs::remove_dir_all(&store).ok();
    }

    #[test]
    fn upsert_external_adds_then_is_idempotent() {
        let mut rf = RegistryFile::default();
        // first call adds an external (managed:false) record
        assert!(upsert_external(
            &mut rf,
            "skill",
            "groundwork",
            "/ext/groundwork"
        ));
        assert_eq!(rf.entries.len(), 1);
        assert!(!rf.entries[0].provenance.managed);
        assert_eq!(rf.entries[0].provenance.canonical_path, "/ext/groundwork");
        // second identical call is a no-op (no change)
        assert!(!upsert_external(
            &mut rf,
            "skill",
            "groundwork",
            "/ext/groundwork"
        ));
        assert_eq!(rf.entries.len(), 1);
        // a moved canonical updates in place + reports a change
        assert!(upsert_external(
            &mut rf,
            "skill",
            "groundwork",
            "/ext2/groundwork"
        ));
        assert_eq!(rf.entries[0].provenance.canonical_path, "/ext2/groundwork");
        assert!(!rf.entries[0].provenance.managed);
    }

    #[test]
    fn upsert_external_flips_a_managed_entry_to_external() {
        let store = tmp("upsert_flip");
        let mut rf = RegistryFile::default();
        rf.entries.push(entry(
            "skill",
            "x",
            &store,
            RegistryProvenance::local(store.join("skills/x").to_string_lossy().into()),
        ));
        assert!(rf.entries[0].provenance.managed, "starts managed");
        assert!(upsert_external(&mut rf, "skill", "x", "/elsewhere/x"));
        assert!(!rf.entries[0].provenance.managed, "now external");
        assert_eq!(rf.entries[0].provenance.canonical_path, "/elsewhere/x");
        std::fs::remove_dir_all(&store).ok();
    }

    #[test]
    fn forget_removes_record_and_reports_presence() {
        let store = tmp("forget");
        let mut rf = RegistryFile::default();
        rf.entries.push(entry(
            "skill",
            "groundwork",
            &store,
            RegistryProvenance::local(store.join("skills/groundwork").to_string_lossy().into()),
        ));
        assert!(forget(&mut rf, "skill", "groundwork"), "record existed");
        assert!(rf.entries.is_empty());
        // forgetting an absent record reports false (idempotent)
        assert!(!forget(&mut rf, "skill", "groundwork"));
        std::fs::remove_dir_all(&store).ok();
    }

    #[test]
    fn backfill_synthesizes_local_for_every_entry() {
        let store = tmp("backfill");
        let entries = vec![
            entry(
                "skill",
                "a",
                &store,
                RegistryProvenance::local(store.join("skills/a").to_string_lossy().into()),
            ),
            entry(
                "agent",
                "b",
                &store,
                RegistryProvenance::local(store.join("agents/b").to_string_lossy().into()),
            ),
        ];
        let rf = backfill_local(&entries);
        assert_eq!(rf.entries.len(), 2);
        assert!(rf
            .entries
            .iter()
            .all(|e| e.provenance.source == ProvenanceSource::Local && e.provenance.managed));
        std::fs::remove_dir_all(&store).ok();
    }
}
