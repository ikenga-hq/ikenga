//! R57 — git/npx primitives in the Store: the pure half.
//!
//! Everything here is filesystem-local and network-free, so it is unit-tested
//! against tempdirs; the fetching half (clone / fetch-at-SHA / `npx skills
//! add`) lives in `install.rs`, which calls into this module.
//!
//! - [`Pin`] — what an install or update must match (N-C): a commit SHA and/or
//!   a content hash. A mismatch is refused with an error carrying
//!   [`PIN_MISMATCH`], which the FE matches on.
//! - [`content_hash_dir`] / [`content_hash_bytes`] — the content hash the
//!   catalog pins (Q3) and the installer verifies. The algorithm is mirrored
//!   by the catalog generator (`scripts/primitives-catalog-pin.ts`); the golden
//!   test below and the generator's test pin the same value.
//! - [`classify_source`] — a pasted source → the git or npx route (N-B).
//! - [`infer_in_tree`] — what a fetched tree holds, for a dry-run resolve (N-B).
//! - [`read_fragment`] — hook/mcp settings fragments found in a fetched tree
//!   (N-A), validated before anything reaches the vault.
#![cfg_attr(not(feature = "desktop"), allow(dead_code))]

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{HookFragment, Kind, ProvenanceSource};

/// Substring every pin refusal carries. Mirrored by `OBA_PIN_MISMATCH` in
/// `tauri-cmd.ts` — change both or neither.
pub(crate) const PIN_MISMATCH: &str = "pin mismatch";

/// Upper bound on the `files` list a resolve returns (display only).
pub(crate) const MAX_LISTED_FILES: usize = 200;

/// What an install or update must match (R57 · N-C). Both halves optional;
/// an all-`None` pin means "unpinned" (legacy behaviour: whatever the source
/// serves at the ref).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Pin {
    /// A commit SHA (full, or a ≥7-char hex prefix). With it, the installer
    /// fetches exactly this commit — never HEAD.
    pub sha: Option<String>,
    /// A content hash (`sha256-<hex>`) the fetched primitive must hash to.
    pub hash: Option<String>,
}

impl Pin {
    /// Build from the wire's optional strings; blank strings count as absent.
    /// A `sha` that is not hex is rejected here, before any fetch.
    pub(crate) fn from_wire(sha: Option<String>, hash: Option<String>) -> Result<Pin, String> {
        let clean = |s: Option<String>| s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let sha = clean(sha).map(|s| s.to_ascii_lowercase());
        let hash = clean(hash);
        if let Some(s) = &sha {
            if !looks_like_sha(s) {
                return Err(format!(
                    "expected SHA {s:?} is not a commit SHA (7–40 hex characters)"
                ));
            }
        }
        if let Some(h) = &hash {
            if !looks_like_content_hash(h) {
                return Err(format!(
                    "expected hash {h:?} is not a content hash (sha256-<64 hex>)"
                ));
            }
        }
        Ok(Pin { sha, hash })
    }

    pub(crate) fn is_pinned(&self) -> bool {
        self.sha.is_some() || self.hash.is_some()
    }

    /// Refuse unless `actual_sha` is the pinned commit. `None` actual (npx with
    /// no resolvable SHA) only passes when no SHA was pinned.
    pub(crate) fn check_sha(&self, actual_sha: Option<&str>, what: &str) -> Result<(), String> {
        let Some(want) = &self.sha else { return Ok(()) };
        match actual_sha {
            Some(got) if sha_matches(got, want) => Ok(()),
            Some(got) => Err(format!(
                "{PIN_MISMATCH}: {what} was reviewed at {} but the source served {} — nothing was written",
                short_sha(want),
                short_sha(got)
            )),
            None => Err(format!(
                "{PIN_MISMATCH}: {what} was reviewed at {} but the source's commit could not be read — nothing was written",
                short_sha(want)
            )),
        }
    }

    /// Refuse unless `actual_hash` is the pinned content hash.
    pub(crate) fn check_hash(&self, actual_hash: &str, what: &str) -> Result<(), String> {
        let Some(want) = &self.hash else {
            return Ok(());
        };
        if want.eq_ignore_ascii_case(actual_hash) {
            Ok(())
        } else {
            Err(format!(
                "{PIN_MISMATCH}: {what}'s content does not match the reviewed hash ({} ≠ {}) — nothing was written",
                short_hash(want),
                short_hash(actual_hash)
            ))
        }
    }
}

/// 7–40 hex chars. (A branch literally named like a SHA is not supported as a
/// pin — pins are commits.)
pub(crate) fn looks_like_sha(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn looks_like_content_hash(s: &str) -> bool {
    s.strip_prefix("sha256-")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Two SHAs name the same commit when one is a (≥7-char) prefix of the other.
pub(crate) fn sha_matches(a: &str, b: &str) -> bool {
    let a = a.trim().to_ascii_lowercase();
    let b = b.trim().to_ascii_lowercase();
    if a.len() < 7 || b.len() < 7 {
        return false;
    }
    a.starts_with(&b) || b.starts_with(&a)
}

pub(crate) fn short_sha(s: &str) -> &str {
    &s[..s.len().min(7)]
}

fn short_hash(s: &str) -> String {
    let body = s.strip_prefix("sha256-").unwrap_or(s);
    format!("sha256-{}…", &body[..body.len().min(10)])
}

// ─── content hash ─────────────────────────────────────────────────────────────
//
// sha256 over the sorted list of files the primitive consists of:
//
//   for each (rel, bytes) sorted by rel (byte order):
//       H.update(rel) ; H.update("\0") ; H.update(hex(sha256(bytes))) ; H.update("\n")
//   => "sha256-" + hex(H)
//
// `rel` is the '/'-separated path relative to the primitive's root; a
// single-file primitive (agent/command `.md`, hook/mcp fragment) hashes one
// entry with `rel = ""`, so the hash never depends on the name it is installed
// under. `.git/` is skipped. Mirrored by `scripts/primitives-catalog-pin.ts`.

fn hash_entries(mut entries: Vec<(String, Vec<u8>)>) -> String {
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut h = Sha256::new();
    for (rel, bytes) in &entries {
        h.update(rel.as_bytes());
        h.update([0u8]);
        h.update(hex::encode(Sha256::digest(bytes)).as_bytes());
        h.update(b"\n");
    }
    format!("sha256-{}", hex::encode(h.finalize()))
}

/// Content hash of a single-file primitive's bytes.
pub(crate) fn content_hash_bytes(bytes: &[u8]) -> String {
    hash_entries(vec![(String::new(), bytes.to_vec())])
}

/// Every regular file under `dir` (following symlinks, skipping `.git`), as
/// `('/'-relative path, absolute path)`, sorted.
pub(crate) fn walk_files(dir: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    fn walk(root: &Path, cur: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
        let rd = std::fs::read_dir(cur).map_err(|e| format!("read {}: {e}", cur.display()))?;
        for e in rd.flatten() {
            let p = e.path();
            if e.file_name() == ".git" {
                continue;
            }
            let meta = std::fs::metadata(&p).map_err(|e| format!("stat {}: {e}", p.display()))?;
            if meta.is_dir() {
                walk(root, &p, out)?;
            } else if meta.is_file() {
                let rel = p
                    .strip_prefix(root)
                    .map_err(|e| e.to_string())?
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push((rel, p));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    out.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    Ok(out)
}

/// Content hash of a directory primitive (a skill).
pub(crate) fn content_hash_dir(dir: &Path) -> Result<String, String> {
    let mut entries = Vec::new();
    for (rel, abs) in walk_files(dir)? {
        let bytes = std::fs::read(&abs).map_err(|e| format!("read {}: {e}", abs.display()))?;
        entries.push((rel, bytes));
    }
    Ok(hash_entries(entries))
}

// ─── source classification (N-B) ──────────────────────────────────────────────

/// A pasted source → `(route, url-or-spec)`.
///
/// - `https://…`, `http://…`, `git@…`, `ssh://…`, `file://…`, or anything
///   ending `.git` → **git** (the clone URL, as given).
/// - `owner/repo`, `github:owner/repo`, or the same after an `npx skills add `
///   prefix → **npx** (the spec, without `github:`).
///
/// Anything else is refused with a message saying what is accepted.
pub(crate) fn classify_source(input: &str) -> Result<(ProvenanceSource, String), String> {
    let mut s = input.trim();
    for prefix in [
        "npx --yes skills add ",
        "npx -y skills add ",
        "npx skills add ",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim();
            break;
        }
    }
    if s.is_empty() {
        return Err("enter a git URL or an owner/repo spec".to_string());
    }
    let is_git = ["https://", "http://", "git@", "ssh://", "file://"]
        .iter()
        .any(|p| s.starts_with(p))
        || s.ends_with(".git");
    if is_git {
        return Ok((ProvenanceSource::Git, s.to_string()));
    }
    let spec = s.strip_prefix("github:").unwrap_or(s);
    let parts: Vec<&str> = spec.split('/').collect();
    let seg_ok = |p: &str| {
        !p.is_empty()
            && !p.starts_with('.')
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    };
    if parts.len() == 2 && parts.iter().all(|p| seg_ok(p)) {
        return Ok((ProvenanceSource::Npx, spec.to_string()));
    }
    Err(format!(
        "{input:?} is not a git URL (https://…, git@…, file://…) or an owner/repo spec"
    ))
}

/// The last path segment of a URL or spec, without `.git` — the default name
/// for a primitive fetched from it.
pub(crate) fn url_leaf(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let leaf = trimmed.rsplit(['/', ':']).next().unwrap_or(trimmed);
    leaf.strip_suffix(".git").unwrap_or(leaf).to_string()
}

// ─── hook / mcp fragments (N-A) ───────────────────────────────────────────────

/// Validate a hook fragment's bytes: it must parse as the vault's
/// `HookFragment` shape (`event` + `block`, optional `file`).
pub(crate) fn validate_hook_fragment(bytes: &[u8], origin: &str) -> Result<(), String> {
    serde_json::from_slice::<HookFragment>(bytes)
        .map(|_| ())
        .map_err(|e| format!("{origin} is not a hook fragment ({{event, block, file?}}): {e}"))
}

/// An MCP server definition from `bytes`. Accepts a bare server def
/// (`{command,…}` / `{url,…}` / `{type,…}`) or a `{mcpServers:{<name>:def}}`
/// wrapper (e.g. a repo's `.mcp.json`), from which `name` is extracted (or the
/// single server when `name` is `None`). Returns `(name?, pretty def bytes)`.
pub(crate) fn extract_mcp_def(
    bytes: &[u8],
    name: Option<&str>,
    origin: &str,
) -> Result<(Option<String>, Vec<u8>), String> {
    let v: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("{origin} is not valid JSON: {e}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| format!("{origin} is not a JSON object"))?;
    if let Some(servers) = obj.get("mcpServers").and_then(|s| s.as_object()) {
        let (key, def) = match name {
            Some(n) => (
                n.to_string(),
                servers
                    .get(n)
                    .ok_or_else(|| format!("{origin} has no mcpServers.{n}"))?,
            ),
            None if servers.len() == 1 => {
                let (k, d) = servers.iter().next().expect("len 1");
                (k.clone(), d)
            }
            None => {
                return Err(format!(
                    "{origin} defines {} MCP servers ({}) — enter a Name to pick one",
                    servers.len(),
                    servers.keys().cloned().collect::<Vec<_>>().join(", ")
                ))
            }
        };
        check_mcp_def(def, origin)?;
        let pretty = serde_json::to_vec_pretty(def).map_err(|e| e.to_string())?;
        return Ok((Some(key), pretty));
    }
    check_mcp_def(&v, origin)?;
    Ok((None, bytes.to_vec()))
}

fn check_mcp_def(def: &serde_json::Value, origin: &str) -> Result<(), String> {
    let o = def
        .as_object()
        .ok_or_else(|| format!("{origin}: an MCP server definition must be an object"))?;
    if ["command", "url", "type"]
        .iter()
        .any(|k| o.contains_key(*k))
    {
        Ok(())
    } else {
        Err(format!(
            "{origin} is not an MCP server definition (needs `command`, `url` or `type`)"
        ))
    }
}

/// A fragment's human description: its top-level `description` string, if any.
pub(crate) fn fragment_description(bytes: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    v.get("description")?.as_str().map(|s| s.to_string())
}

/// Read + validate a fragment of `kind` at `path` (the whole file is the
/// fragment; for mcp a `{mcpServers}` wrapper is unwrapped by `name`).
pub(crate) fn read_fragment(
    path: &Path,
    kind: Kind,
    name: Option<&str>,
) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let origin = path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    match kind {
        Kind::Hook => {
            validate_hook_fragment(&bytes, &origin)?;
            Ok(bytes)
        }
        Kind::Mcp => extract_mcp_def(&bytes, name, &origin).map(|(_, b)| b),
        other => Err(format!(
            "kind {} is not a settings fragment",
            other.as_str()
        )),
    }
}

// ─── kind inference (N-B) ─────────────────────────────────────────────────────

/// Where a primitive sits inside a fetched tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Found {
    /// A skill dir or an agent/command `.md` file.
    Path(PathBuf),
    /// A validated hook/mcp fragment (the exact bytes the vault will hold).
    Fragment(Vec<u8>),
}

/// One primitive candidate found in a tree.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub kind: Kind,
    pub name: String,
    /// How it was found, e.g. `root SKILL.md`, `agents/<n>.md`.
    pub how: String,
    pub found: Found,
}

fn md_stems(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let fname = e.file_name().to_string_lossy().to_string();
            if p.is_file() {
                if let Some(stem) = fname.strip_suffix(".md") {
                    if !stem.starts_with('.') && !stem.eq_ignore_ascii_case("readme") {
                        out.push((stem.to_string(), p));
                    }
                }
            }
        }
    }
    out.sort();
    out
}

fn json_stems(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let fname = e.file_name().to_string_lossy().to_string();
            if p.is_file() {
                if let Some(stem) = fname.strip_suffix(".json") {
                    if !stem.starts_with('.') {
                        out.push((stem.to_string(), p));
                    }
                }
            }
        }
    }
    out.sort();
    out
}

/// The name a skill-at-root declares in its SKILL.md frontmatter, if any.
fn skill_frontmatter_name(skill_dir: &Path) -> Option<String> {
    let (fm, _) =
        crate::server::shared::claude_config::parse_md(&skill_dir.join("SKILL.md")).ok()?;
    crate::server::shared::claude_config::string_field(&fm, "name")
        .filter(|n| super::validate_name(n).is_ok())
}

/// Every primitive a fetched tree plausibly holds, in a bounded, predictable
/// search (no deep guessing):
///
/// - skill   → `SKILL.md` at the root · `skills/*/` · `.claude/skills/*/` · `.agents/skills/*/`
/// - agent   → `agents/*.md` · `.claude/agents/*.md`
/// - command → `commands/*.md` · `.claude/commands/*.md`
/// - hook    → `hooks/*.json`
/// - mcp     → `mcp/*.json` · each `mcpServers` key of a root `.mcp.json`
///
/// Invalid hook/mcp JSON is skipped here (it simply isn't a candidate).
pub(crate) fn candidates_in_tree(root: &Path, url_leaf_name: &str) -> Vec<Candidate> {
    let mut out = Vec::new();
    if root.join("SKILL.md").is_file() {
        let name = skill_frontmatter_name(root).unwrap_or_else(|| url_leaf_name.to_string());
        out.push(Candidate {
            kind: Kind::Skill,
            name,
            how: "root SKILL.md".into(),
            found: Found::Path(root.to_path_buf()),
        });
    }
    for base in ["skills", ".claude/skills", ".agents/skills"] {
        let dir = root.join(base);
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut v: Vec<_> = rd.flatten().map(|e| e.path()).collect();
            v.sort();
            for p in v {
                if p.join("SKILL.md").is_file() {
                    let n = p
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    if n.starts_with('.') {
                        continue;
                    }
                    out.push(Candidate {
                        kind: Kind::Skill,
                        how: format!("{base}/{n}/SKILL.md"),
                        name: n,
                        found: Found::Path(p),
                    });
                }
            }
        }
    }
    for (kind, bases) in [
        (Kind::Agent, ["agents", ".claude/agents"]),
        (Kind::Command, ["commands", ".claude/commands"]),
    ] {
        for base in bases {
            for (stem, p) in md_stems(&root.join(base)) {
                out.push(Candidate {
                    kind,
                    how: format!("{base}/{stem}.md"),
                    name: stem,
                    found: Found::Path(p),
                });
            }
        }
    }
    for (stem, p) in json_stems(&root.join("hooks")) {
        if let Ok(bytes) = read_fragment(&p, Kind::Hook, None) {
            out.push(Candidate {
                kind: Kind::Hook,
                how: format!("hooks/{stem}.json"),
                name: stem,
                found: Found::Fragment(bytes),
            });
        }
    }
    for (stem, p) in json_stems(&root.join("mcp")) {
        if let Ok(bytes) = read_fragment(&p, Kind::Mcp, Some(&stem)) {
            out.push(Candidate {
                kind: Kind::Mcp,
                how: format!("mcp/{stem}.json"),
                name: stem,
                found: Found::Fragment(bytes),
            });
        }
    }
    let dot_mcp = root.join(".mcp.json");
    if let Ok(raw) = std::fs::read(&dot_mcp) {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) {
            if let Some(servers) = v.get("mcpServers").and_then(|s| s.as_object()) {
                for (k, def) in servers {
                    if check_mcp_def(def, ".mcp.json").is_ok() {
                        if let Ok(pretty) = serde_json::to_vec_pretty(def) {
                            out.push(Candidate {
                                kind: Kind::Mcp,
                                name: k.clone(),
                                how: format!(".mcp.json · mcpServers.{k}"),
                                found: Found::Fragment(pretty),
                            });
                        }
                    }
                }
            }
        }
    }
    out
}

/// Pick the one primitive a dry-run resolve reports. `kind` / `name` narrow the
/// candidates; a root `SKILL.md` wins when a skill is possible (the root IS the
/// skill — the same precedence the installer's `locate_in_clone` uses, and it
/// then takes the requested `name`). Zero or several remaining candidates is an
/// error that says what was looked for or found, so the user can narrow it.
pub(crate) fn infer_in_tree(
    root: &Path,
    kind: Option<Kind>,
    name: Option<&str>,
    url_leaf_name: &str,
) -> Result<Candidate, String> {
    let all = candidates_in_tree(root, url_leaf_name);
    let kind_ok = |c: &Candidate| kind.is_none_or(|k| k == c.kind);
    if let Some(root_skill) = all.iter().find(|c| c.how == "root SKILL.md" && kind_ok(c)) {
        let mut c = root_skill.clone();
        if let Some(n) = name {
            c.name = n.to_string();
        }
        return Ok(c);
    }
    let matching: Vec<&Candidate> = all
        .iter()
        .filter(|c| kind_ok(c) && name.is_none_or(|n| n == c.name))
        .collect();
    match matching.as_slice() {
        [one] => Ok((*one).clone()),
        [] => {
            let what = match (kind, name) {
                (Some(k), Some(n)) => format!("{} {n:?}", k.as_str()),
                (Some(k), None) => format!("a {}", k.as_str()),
                (None, Some(n)) => format!("anything named {n:?}"),
                (None, None) => "anything installable".to_string(),
            };
            let found = if all.is_empty() {
                String::new()
            } else {
                format!(
                    " (it holds: {})",
                    all.iter()
                        .map(|c| format!("{} {}", c.kind.as_str(), c.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            Err(format!(
                "found no {what} in the fetched source — looked for SKILL.md at the root and in \
                 skills/, agents/*.md, commands/*.md, hooks/*.json, mcp/*.json and .mcp.json{found}"
            ))
        }
        many => Err(format!(
            "the source holds {} primitives ({}) — pick a Kind and Name",
            many.len(),
            many.iter()
                .map(|c| format!("{} {}", c.kind.as_str(), c.name))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Locate `kind`/`name` for an INSTALL of a hook or MCP fragment (N-A):
/// `hooks/<name>.json` · `<name>.json` for a hook; `mcp/<name>.json` ·
/// `<name>.json` · `.mcp.json`'s `mcpServers.<name>` for an MCP server.
pub(crate) fn locate_fragment(root: &Path, kind: Kind, name: &str) -> Result<Vec<u8>, String> {
    let (sub, extra_dot_mcp) = match kind {
        Kind::Hook => ("hooks", false),
        Kind::Mcp => ("mcp", true),
        other => {
            return Err(format!(
                "kind {} is not a settings fragment",
                other.as_str()
            ))
        }
    };
    let leaf = format!("{name}.json");
    for cand in [root.join(sub).join(&leaf), root.join(&leaf)] {
        if cand.is_file() {
            return read_fragment(&cand, kind, Some(name));
        }
    }
    if extra_dot_mcp && root.join(".mcp.json").is_file() {
        return read_fragment(&root.join(".mcp.json"), kind, Some(name));
    }
    Err(format!(
        "no {} {name:?} found in the fetched source (looked at {sub}/{leaf}, {leaf}{})",
        kind.as_str(),
        if extra_dot_mcp { ", .mcp.json" } else { "" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("oba-source-{tag}-{nonce}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// GOLDEN — `scripts/primitives-catalog-pin.test.ts` asserts the same
    /// value for the same tree. If this changes, the catalog's pinned hashes
    /// all stop verifying: change both, and re-pin the catalog.
    #[test]
    fn content_hash_dir_golden() {
        let d = tmp("golden");
        std::fs::write(d.join("SKILL.md"), "---\nname: demo\n---\nbody\n").unwrap();
        std::fs::create_dir_all(d.join("refs")).unwrap();
        std::fs::write(d.join("refs/a.txt"), "alpha").unwrap();
        std::fs::create_dir_all(d.join(".git")).unwrap();
        std::fs::write(d.join(".git/HEAD"), "ignored").unwrap();
        assert_eq!(
            content_hash_dir(&d).unwrap(),
            "sha256-a455daeda4518fad52af890f26b404ecaa0661410226ac0717cd15ee69ba7df6"
        );
        assert_eq!(
            content_hash_bytes(b"{\"event\":\"PreToolUse\",\"block\":[]}"),
            "sha256-c784302747a253d95f356d0a4085272693696795e9122c79fefcda93ff1a2195"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn content_hash_changes_with_content_and_path_not_dir_name() {
        let a = tmp("ha");
        let b = tmp("hb");
        for d in [&a, &b] {
            std::fs::write(d.join("SKILL.md"), "x").unwrap();
        }
        assert_eq!(content_hash_dir(&a).unwrap(), content_hash_dir(&b).unwrap());
        std::fs::write(b.join("SKILL.md"), "y").unwrap();
        assert_ne!(content_hash_dir(&a).unwrap(), content_hash_dir(&b).unwrap());
        std::fs::write(b.join("SKILL.md"), "x").unwrap();
        std::fs::write(b.join("extra.md"), "").unwrap();
        assert_ne!(content_hash_dir(&a).unwrap(), content_hash_dir(&b).unwrap());
        std::fs::remove_dir_all(&a).ok();
        std::fs::remove_dir_all(&b).ok();
    }

    #[test]
    fn pin_from_wire_validates_and_normalizes() {
        let p = Pin::from_wire(Some(" ABCDEF1 ".into()), Some(String::new())).unwrap();
        assert_eq!(p.sha.as_deref(), Some("abcdef1"));
        assert_eq!(p.hash, None);
        assert!(p.is_pinned());
        assert!(!Pin::from_wire(None, None).unwrap().is_pinned());
        assert!(Pin::from_wire(Some("main".into()), None).is_err());
        assert!(Pin::from_wire(None, Some("md5-abc".into())).is_err());
    }

    #[test]
    fn pin_checks_refuse_with_the_mismatch_marker() {
        let p = Pin {
            sha: Some("9c41e07".into()),
            hash: Some(format!("sha256-{}", "a".repeat(64))),
        };
        assert!(p
            .check_sha(Some("9c41e07aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), "x")
            .is_ok());
        let e = p.check_sha(Some("8b77f2d0"), "skill x").unwrap_err();
        assert!(
            e.contains(PIN_MISMATCH) && e.contains("9c41e07") && e.contains("8b77f2d"),
            "{e}"
        );
        assert!(p.check_sha(None, "x").unwrap_err().contains(PIN_MISMATCH));
        assert!(p
            .check_hash(&format!("sha256-{}", "a".repeat(64)), "x")
            .is_ok());
        assert!(p
            .check_hash(&format!("sha256-{}", "b".repeat(64)), "x")
            .unwrap_err()
            .contains(PIN_MISMATCH));
        // an unpinned pin accepts anything
        assert!(Pin::default().check_sha(Some("0000000"), "x").is_ok());
        assert!(Pin::default().check_hash("sha256-x", "x").is_ok());
    }

    #[test]
    fn sha_prefix_matching() {
        assert!(sha_matches("abcdef1234", "ABCDEF1"));
        assert!(!sha_matches("abcdef1234", "abcdef2"));
        assert!(!sha_matches("abc", "abc"), "too short to be a pin");
    }

    #[test]
    fn classify_source_routes_git_and_npx() {
        use ProvenanceSource::*;
        assert_eq!(
            classify_source("https://github.com/o/r").unwrap(),
            (Git, "https://github.com/o/r".into())
        );
        assert_eq!(classify_source("git@github.com:o/r.git").unwrap().0, Git);
        assert_eq!(classify_source("file:///tmp/x").unwrap().0, Git);
        assert_eq!(classify_source("o/r").unwrap(), (Npx, "o/r".into()));
        assert_eq!(classify_source("github:o/r").unwrap(), (Npx, "o/r".into()));
        assert_eq!(
            classify_source("  npx skills add royalti-io/groundwork ").unwrap(),
            (Npx, "royalti-io/groundwork".into())
        );
        assert!(classify_source("").is_err());
        assert!(classify_source("just words").is_err());
        assert!(classify_source("a/b/c").is_err());
        assert!(classify_source("../etc").is_err());
    }

    #[test]
    fn url_leaf_strips_git_suffix() {
        assert_eq!(
            url_leaf("https://github.com/o/claude-hooks.git"),
            "claude-hooks"
        );
        assert_eq!(url_leaf("git@github.com:o/r.git"), "r");
        assert_eq!(url_leaf("o/groundwork"), "groundwork");
        assert_eq!(url_leaf("https://x/y/"), "y");
    }

    #[test]
    fn infer_prefers_root_skill_and_takes_frontmatter_name() {
        let d = tmp("inf_root");
        std::fs::write(
            d.join("SKILL.md"),
            "---\nname: from-fm\ndescription: d\n---\n",
        )
        .unwrap();
        std::fs::create_dir_all(d.join("agents")).unwrap();
        std::fs::write(d.join("agents/helper.md"), "x").unwrap();
        let c = infer_in_tree(&d, None, None, "leaf").unwrap();
        assert_eq!(
            (c.kind, c.name.as_str(), c.how.as_str()),
            (Kind::Skill, "from-fm", "root SKILL.md")
        );
        // an explicit name renames the root skill
        assert_eq!(
            infer_in_tree(&d, None, Some("mine"), "leaf").unwrap().name,
            "mine"
        );
        // a kind filter reaches past the root skill
        let a = infer_in_tree(&d, Some(Kind::Agent), None, "leaf").unwrap();
        assert_eq!((a.kind, a.name.as_str()), (Kind::Agent, "helper"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn infer_reports_ambiguity_and_absence() {
        let d = tmp("inf_many");
        std::fs::create_dir_all(d.join("commands")).unwrap();
        std::fs::write(d.join("commands/a.md"), "x").unwrap();
        std::fs::write(d.join("commands/b.md"), "x").unwrap();
        let e = infer_in_tree(&d, None, None, "leaf").unwrap_err();
        assert!(e.contains("2 primitives") && e.contains("command a"), "{e}");
        assert_eq!(
            infer_in_tree(&d, None, Some("b"), "leaf").unwrap().name,
            "b"
        );
        let e = infer_in_tree(&d, Some(Kind::Skill), None, "leaf").unwrap_err();
        assert!(
            e.contains("found no a skill") && e.contains("command a"),
            "{e}"
        );
        let empty = tmp("inf_empty");
        assert!(infer_in_tree(&empty, None, None, "leaf")
            .unwrap_err()
            .contains("anything installable"));
        std::fs::remove_dir_all(&d).ok();
        std::fs::remove_dir_all(&empty).ok();
    }

    #[test]
    fn infer_finds_hooks_and_mcp_and_skips_invalid_json() {
        let d = tmp("inf_frag");
        std::fs::create_dir_all(d.join("hooks")).unwrap();
        std::fs::write(
            d.join("hooks/floor.json"),
            r#"{"event":"PreToolUse","block":[{"matcher":"Task","hooks":[]}]}"#,
        )
        .unwrap();
        std::fs::write(d.join("hooks/broken.json"), r#"{"nope":1}"#).unwrap();
        std::fs::write(
            d.join(".mcp.json"),
            r#"{"mcpServers":{"royalti":{"command":"node","args":["s.js"]}}}"#,
        )
        .unwrap();
        let h = infer_in_tree(&d, Some(Kind::Hook), None, "leaf").unwrap();
        assert_eq!((h.kind, h.name.as_str()), (Kind::Hook, "floor"));
        let m = infer_in_tree(&d, Some(Kind::Mcp), None, "leaf").unwrap();
        assert_eq!(m.name, "royalti");
        let Found::Fragment(bytes) = m.found else {
            panic!("fragment")
        };
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["command"], "node");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn locate_fragment_reads_hook_and_unwraps_mcp() {
        let d = tmp("loc_frag");
        std::fs::create_dir_all(d.join("hooks")).unwrap();
        let hook = r#"{"event":"PreToolUse","block":[]}"#;
        std::fs::write(d.join("hooks/h.json"), hook).unwrap();
        assert_eq!(
            locate_fragment(&d, Kind::Hook, "h").unwrap(),
            hook.as_bytes()
        );
        std::fs::write(
            d.join(".mcp.json"),
            r#"{"mcpServers":{"a":{"url":"http://x"},"b":{"command":"y"}}}"#,
        )
        .unwrap();
        let b = locate_fragment(&d, Kind::Mcp, "b").unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&b).unwrap()["command"],
            "y"
        );
        assert!(locate_fragment(&d, Kind::Mcp, "zzz").is_err());
        assert!(locate_fragment(&d, Kind::Hook, "missing")
            .unwrap_err()
            .contains("hooks/missing.json"));
        // an invalid hook is refused before it could reach the vault
        std::fs::write(d.join("hooks/bad.json"), r#"{"event":1}"#).unwrap();
        assert!(locate_fragment(&d, Kind::Hook, "bad").is_err());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn extract_mcp_def_requires_a_name_for_several_servers() {
        let two = br#"{"mcpServers":{"a":{"command":"x"},"b":{"command":"y"}}}"#;
        assert!(extract_mcp_def(two, None, "f")
            .unwrap_err()
            .contains("enter a Name"));
        // GOLDEN with `catalog-pin.golden.test.ts`: an unwrapped def is written
        // key-sorted, 2-space pretty — the generator hashes these exact bytes.
        let wrapped = br#"{"mcpServers":{"r":{"command":"node","args":["a"]}}}"#;
        let (n, bytes) = extract_mcp_def(wrapped, Some("r"), "f").unwrap();
        assert_eq!(n.as_deref(), Some("r"));
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "{\n  \"args\": [\n    \"a\"\n  ],\n  \"command\": \"node\"\n}"
        );
        let bare = br#"{"command":"x"}"#;
        assert_eq!(
            extract_mcp_def(bare, None, "f").unwrap(),
            (None, bare.to_vec())
        );
        assert!(extract_mcp_def(br#"{"foo":1}"#, None, "f").is_err());
    }
}
