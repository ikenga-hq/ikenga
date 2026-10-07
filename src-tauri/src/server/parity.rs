//! Tauri-command ↔ daemon-RPC parity ratchet (WP-19).
//!
//! Every command the desktop registers in `commands/registry.rs`'s
//! `tauri::generate_handler![...]` must be exactly one of:
//!
//! * **served** — a string literal in an arm pattern of `rpc.rs`'s
//!   `match payload.cmd.as_str() { ... }` (a decided refusal arm counts: it
//!   is an answer, not the unknown-command fallthrough), or
//! * **allowlisted** — a table in `desktop_only.toml` with a valid tag and a
//!   non-empty reason.
//!
//! Never both, never neither, and no allowlist entry may name something that
//! is not a Tauri command. The allowlist only ever shrinks; see its header.
//!
//! Both inputs are read as text via `include_str!`, so this compiles — and
//! gates — in the desktop build and in the daemon's `--no-default-features`
//! build alike. The `generate_handler!` parse is the same one
//! `scripts/check-acl-parity.ts` does, so the two gates agree on the list.
//! The list moved out of `lib.rs` in the WP-19 final slice (part A); both
//! gates also assert it is the only one — `lib.rs` has none — so a second
//! handler list cannot bypass them.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

/// The one `generate_handler!` list (WP-19 final slice A).
const REGISTRY_RS: &str = include_str!("../commands/registry.rs");
/// Read only to assert it holds NO handler list any more.
const LIB_RS: &str = include_str!("../lib.rs");
/// The literal both gates anchor on.
const HANDLER_OPEN: &str = "tauri::generate_handler![";
const RPC_RS: &str = include_str!("rpc.rs");
const ALLOWLIST: &str = include_str!("desktop_only.toml");

const VALID_TAGS: &[&str] = &["WP-19", "WP-18b", "WP-20", "desktop-only-forever"];

/// Verbs the daemon serves that are NOT Tauri commands: browser-transport
/// stand-ins with no desktop counterpart. Listed explicitly so that a typo'd
/// arm name (which would also be "served but not a Tauri command") cannot
/// hide — the served-minus-Tauri set must equal this exactly.
///
/// * `pty_list` — legacy alias of `pty_terminal_list`, same arm.
/// * `fs_home` — the browser has no `@tauri-apps/api/path` `homeDir()`.
/// * `secrets_default_names` (WP-78a, review WP76-RV1) — the operator
///   default's names, a layer only the daemon has (the desktop keychain has
///   no `IKENGA_SECRET_*` default).
/// * G-ACCESS §9.1 (WP-74a): `notifications_record_access` and
///   `share_project_info` are `internal` arms (broker → owner child only);
///   the three `permission_relay_*` arms are called only by the desktop's
///   own Rust relay task (WP-75), never from the front end.
/// * plans/pwa S2: the six `access_push_*` arms are browser-only; the
///   desktop never registers a service worker, so it has no push
///   subscription to manage.
/// * `server_open_terminals` (WP-P9) — `internal`: the T1 broker asks each
///   running child how many terminals an update restart would end.
/// * `term_hooks_*` (gap audit rank 11) — a daemon terminal's claude hook
///   settings location, statusline snapshots and permission-gate answer. The
///   desktop reaches the same data through the iyke bridge, which a browser
///   has no endpoint for.
const DAEMON_ONLY_VERBS: &[&str] = &[
    "access_push_config",
    "access_push_list",
    "access_push_subscribe",
    "access_push_test",
    "access_push_unsubscribe",
    "access_push_update",
    "fs_home",
    "notifications_record_access",
    "permission_relay_put",
    "permission_relay_resolve",
    "permission_relay_take",
    "pty_list",
    "secrets_default_names",
    "server_open_terminals",
    "share_project_info",
    "term_hooks_decide",
    "term_hooks_info",
    "term_hooks_statusline_snapshot",
];

/// `tauri::generate_handler![ … ]` command names, module paths stripped —
/// line-for-line the parse in `scripts/check-acl-parity.ts`: slice to the
/// first line that starts with the closing `]` (`]` in a fn body, `])` as a
/// builder argument), strip `//` comments, keep lines shaped
/// `path::to::cmd,`. `#[cfg(...)]` attribute lines inside the list don't
/// match and are skipped.
fn tauri_commands(src: &str) -> Vec<String> {
    let start = src
        .find(HANDLER_OPEN)
        .expect("no `tauri::generate_handler![` in commands/registry.rs — parser drifted");
    let close_re = regex::Regex::new(r"\n[ \t]*\]").expect("static regex");
    let end = start
        + close_re
            .find(&src[start..])
            .expect("unterminated generate_handler! in commands/registry.rs")
            .start();
    let line_re = regex::Regex::new(r"^((?:[A-Za-z_][A-Za-z0-9_]*::)*)([a-z_][a-z0-9_]*)\s*,$")
        .expect("static regex");
    src[start..end]
        .lines()
        .filter_map(|raw| {
            let line = raw.split("//").next().unwrap_or("").trim();
            line_re.captures(line).map(|c| c[2].to_string())
        })
        .collect()
}

/// Minimal Rust lexer state for [`served_verbs`]: enough to never mistake a
/// bracket or `=>` inside a string, char literal or comment for code.
struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn peek(&self, off: usize) -> Option<u8> {
        self.src.get(self.pos + off).copied()
    }

    /// If a comment, string or char literal starts at `pos`, skip it and
    /// return its text (for string literals: the unescaped-enough contents,
    /// `Some(Some(..))`; for comments / chars: `Some(None)`).
    fn skip_opaque(&mut self) -> Option<Option<String>> {
        let c = self.peek(0)?;
        match c {
            b'/' if self.peek(1) == Some(b'/') => {
                while let Some(c) = self.peek(0) {
                    if c == b'\n' {
                        break;
                    }
                    self.pos += 1;
                }
                Some(None)
            }
            b'/' if self.peek(1) == Some(b'*') => {
                self.pos += 2;
                while self.pos < self.src.len()
                    && !(self.peek(0) == Some(b'*') && self.peek(1) == Some(b'/'))
                {
                    self.pos += 1;
                }
                self.pos = (self.pos + 2).min(self.src.len());
                Some(None)
            }
            // Raw string: r"…" / r#"…"#. Only when `r` is not the tail of an
            // identifier (`for"` can't occur, but `bar"` is not raw either).
            b'r' if !self.prev_is_ident() && matches!(self.peek(1), Some(b'"') | Some(b'#')) => {
                let mut hashes = 0;
                let mut p = self.pos + 1;
                while self.src.get(p) == Some(&b'#') {
                    hashes += 1;
                    p += 1;
                }
                if self.src.get(p) != Some(&b'"') {
                    return None;
                }
                let body_start = p + 1;
                let mut q = body_start;
                loop {
                    if q >= self.src.len() {
                        self.pos = q;
                        return Some(None);
                    }
                    if self.src[q] == b'"'
                        && self.src[q + 1..]
                            .iter()
                            .take(hashes)
                            .filter(|&&b| b == b'#')
                            .count()
                            == hashes
                    {
                        let s = String::from_utf8_lossy(&self.src[body_start..q]).into_owned();
                        self.pos = q + 1 + hashes;
                        return Some(Some(s));
                    }
                    q += 1;
                }
            }
            b'"' => {
                let body_start = self.pos + 1;
                let mut q = body_start;
                while q < self.src.len() && self.src[q] != b'"' {
                    q += if self.src[q] == b'\\' { 2 } else { 1 };
                }
                let s = String::from_utf8_lossy(&self.src[body_start..q.min(self.src.len())])
                    .into_owned();
                self.pos = (q + 1).min(self.src.len());
                Some(Some(s))
            }
            // Char literal ('x', '\n', '\''), not a lifetime ('a).
            b'\'' => {
                if self.peek(1) == Some(b'\\') {
                    // Skip the escaped char itself (`'\''`), then find the close.
                    let mut q = self.pos + 3;
                    while q < self.src.len() && self.src[q] != b'\'' {
                        q += 1;
                    }
                    self.pos = (q + 1).min(self.src.len());
                    Some(None)
                } else if self.peek(2) == Some(b'\'') {
                    self.pos += 3;
                    Some(None)
                } else {
                    // A lifetime: consume just the quote.
                    self.pos += 1;
                    Some(None)
                }
            }
            _ => None,
        }
    }

    fn prev_is_ident(&self) -> bool {
        self.pos > 0 && {
            let p = self.src[self.pos - 1];
            p.is_ascii_alphanumeric() || p == b'_'
        }
    }

    /// Next byte that is not whitespace or inside a comment.
    fn next_significant(&mut self) -> Option<u8> {
        loop {
            let c = self.peek(0)?;
            if c.is_ascii_whitespace() {
                self.pos += 1;
                continue;
            }
            if c == b'/' && matches!(self.peek(1), Some(b'/') | Some(b'*')) {
                self.skip_opaque();
                continue;
            }
            return Some(c);
        }
    }
}

/// One top-level arm of the dispatch `match`: the string literals in its
/// pattern (empty for an identifier / `_` catch-all).
#[derive(Debug, PartialEq)]
struct Arm {
    literals: Vec<String>,
    pattern: String,
}

/// Parse the arms of the first `match payload.cmd.as_str() {` in `src`.
///
/// Only arm PATTERNS contribute (left of a top-level `=>`); literals in arm
/// bodies, nested matches and comments never do. Handles `"a" =>`,
/// `"a" | "b" =>` and `cmd @ ("a" | "b") =>`. Stops at the match's closing
/// brace. Arm bodies end at a top-level `,`, or at the `}` closing a
/// block-like body when the next token is not a `.`/`?` continuation.
fn dispatch_arms(src: &str) -> Vec<Arm> {
    const HEAD: &str = "match payload.cmd.as_str() {";
    let start = src
        .find(HEAD)
        .expect("no `match payload.cmd.as_str() {` in rpc.rs — parser drifted")
        + HEAD.len();
    let mut lx = Lexer {
        src: src.as_bytes(),
        pos: start,
    };

    let mut arms = Vec::new();
    let mut in_pattern = true;
    let mut pat_text = String::new();
    let mut pat_lits: Vec<String> = Vec::new();
    // Depth of (), [], {} combined, relative to the match body.
    let mut depth: i32 = 0;

    while lx.pos < lx.src.len() {
        let before = lx.pos;
        if let Some(lit) = lx.skip_opaque() {
            if in_pattern {
                if let Some(s) = lit {
                    pat_text.push('"');
                    pat_text.push_str(&s);
                    pat_text.push('"');
                    pat_lits.push(s);
                } else {
                    // Keep token boundaries for a lifetime / comment.
                    pat_text.push(' ');
                }
            }
            debug_assert!(lx.pos > before);
            continue;
        }
        let c = lx.src[lx.pos];
        lx.pos += 1;

        if in_pattern {
            match c {
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b'}' if depth == 0 => break, // end of the match block
                b'=' if depth == 0 && lx.peek(0) == Some(b'>') => {
                    lx.pos += 1;
                    arms.push(Arm {
                        literals: std::mem::take(&mut pat_lits),
                        pattern: std::mem::take(&mut pat_text).trim().to_string(),
                    });
                    in_pattern = false;
                    continue;
                }
                _ => {}
            }
            pat_text.push(c as char);
            continue;
        }

        // In an arm body.
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' => depth -= 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    // A block-like body just closed. Unless it continues
                    // (`match {..}.foo()`, `?`, `if {..} else {..}`), the
                    // arm is over.
                    match lx.next_significant() {
                        Some(b',') => {
                            lx.pos += 1;
                            in_pattern = true;
                        }
                        Some(b'.') | Some(b'?') => {}
                        _ if lx.src[lx.pos..].starts_with(b"else")
                            && !lx
                                .src
                                .get(lx.pos + 4)
                                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_') => {}
                        _ => in_pattern = true,
                    }
                }
            }
            b',' if depth == 0 => in_pattern = true,
            _ => {}
        }
    }
    arms
}

/// The served verbs: every literal in every arm pattern, after asserting the
/// match ends in exactly one literal-free catch-all (the unknown-command
/// fallback) — so a parse that wandered past the match fails loudly.
pub(crate) fn served_verbs(rpc_rs: &str) -> BTreeSet<String> {
    let arms = dispatch_arms(rpc_rs);
    let catch_alls: Vec<&Arm> = arms.iter().filter(|a| a.literals.is_empty()).collect();
    assert_eq!(
        catch_alls.len(),
        1,
        "expected exactly one catch-all arm (the unknown-command fallback) in the rpc.rs \
         dispatch; found patterns without a literal: {:?}",
        catch_alls.iter().map(|a| &a.pattern).collect::<Vec<_>>()
    );
    assert!(
        std::ptr::eq(catch_alls[0], arms.last().unwrap()),
        "the catch-all must be the last arm; got {:?}",
        catch_alls[0].pattern
    );
    arms.into_iter().flat_map(|a| a.literals).collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowEntry {
    tag: String,
    reason: String,
}

fn allowlist() -> BTreeMap<String, AllowEntry> {
    // One table per command: TOML rejects a repeated table header, so a
    // duplicate entry is a parse error here, not a silent overwrite.
    toml::from_str(ALLOWLIST).unwrap_or_else(|e| panic!("desktop_only.toml: {e}"))
}

fn fmt_list<'a>(names: impl IntoIterator<Item = &'a String>) -> String {
    names
        .into_iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

#[test]
fn every_tauri_command_is_served_xor_allowlisted() {
    let tauri_list = tauri_commands(REGISTRY_RS);
    assert!(
        tauri_list.len() > 250,
        "parsed only {} Tauri commands from generate_handler! — the parser has drifted",
        tauri_list.len()
    );
    let mut seen = BTreeSet::new();
    let dups: Vec<&String> = tauri_list.iter().filter(|c| !seen.insert(*c)).collect();
    assert!(
        dups.is_empty(),
        "duplicate Tauri commands in generate_handler!: {}",
        fmt_list(dups)
    );
    let tauri: BTreeSet<String> = tauri_list.into_iter().collect();

    let served = served_verbs(RPC_RS);
    for known in [
        "pty_spawn",
        "db_query",
        "pkg_content_html",
        "pkg_kernel_status",
        "secrets_set_scoped", // inside `cmd @ (… | …)`
    ] {
        assert!(
            served.contains(known),
            "served-set parser lost `{known}` — rpc.rs parse has drifted (found: {})",
            fmt_list(&served)
        );
    }
    for body_literal in ["terminal_id", "pkgId", "workspace", "sql"] {
        assert!(
            !served.contains(body_literal),
            "served-set parser picked up `{body_literal}` from an arm body / helper"
        );
    }

    let allow = allowlist();

    let bad_tags: Vec<String> = allow
        .iter()
        .filter(|(_, e)| !VALID_TAGS.contains(&e.tag.as_str()))
        .map(|(c, e)| format!("{c} (tag {:?})", e.tag))
        .collect();
    assert!(
        bad_tags.is_empty(),
        "desktop_only.toml entries with a tag outside {VALID_TAGS:?}: {}",
        bad_tags.join(", ")
    );
    let empty: Vec<&String> = allow
        .iter()
        .filter(|(_, e)| e.reason.trim().is_empty())
        .map(|(c, _)| c)
        .collect();
    assert!(
        empty.is_empty(),
        "desktop_only.toml entries with an empty reason: {}",
        fmt_list(empty)
    );

    let stale: Vec<&String> = allow.keys().filter(|c| !tauri.contains(*c)).collect();
    assert!(
        stale.is_empty(),
        "desktop_only.toml names commands that are not in generate_handler! (delete them): {}",
        fmt_list(stale)
    );

    let both: Vec<&String> = tauri
        .iter()
        .filter(|c| served.contains(*c) && allow.contains_key(*c))
        .collect();
    assert!(
        both.is_empty(),
        "served by rpc.rs AND still allowlisted — delete these from desktop_only.toml \
         (the ratchet only shrinks): {}",
        fmt_list(both)
    );

    let neither: Vec<&String> = tauri
        .iter()
        .filter(|c| !served.contains(*c) && !allow.contains_key(*c))
        .collect();
    assert!(
        neither.is_empty(),
        "Tauri commands neither served by the daemon (rpc.rs) nor allowlisted in \
         server/desktop_only.toml — add an rpc.rs arm, or a table with an honest tag: {}",
        fmt_list(neither)
    );

    let daemon_only: BTreeSet<String> = served.difference(&tauri).cloned().collect();
    let expected: BTreeSet<String> = DAEMON_ONLY_VERBS.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        daemon_only, expected,
        "rpc.rs arms that are not Tauri commands must be exactly DAEMON_ONLY_VERBS — a \
         mismatch is usually a typo'd arm name"
    );

    let mut per_tag: BTreeMap<&str, usize> = BTreeMap::new();
    for e in allow.values() {
        *per_tag.entry(e.tag.as_str()).or_default() += 1;
    }
    eprintln!(
        "[parity] tauri={} served∩tauri={} daemon_only={} allowlisted={} per_tag={per_tag:?}",
        tauri.len(),
        served.intersection(&tauri).count(),
        daemon_only.len(),
        allow.len()
    );
}

/// The registry is the ONE handler list: `lib.rs` installs it and holds none
/// of its own. A second list anywhere the gates don't parse would register
/// commands neither gate sees. Mirrored in `scripts/check-acl-parity.ts`.
#[test]
fn registry_holds_the_only_handler_list() {
    let in_lib = LIB_RS.matches(HANDLER_OPEN).count();
    let in_registry = REGISTRY_RS.matches(HANDLER_OPEN).count();
    assert_eq!(
        (in_lib, in_registry),
        (0, 1),
        "expected exactly one `tauri::generate_handler![` across lib.rs + commands/registry.rs, \
         all of it in registry.rs; found lib.rs={in_lib} registry.rs={in_registry}"
    );
}

#[test]
fn arm_parser_reads_patterns_not_bodies() {
    let src = r##"
fn helper(x: &str) { match x { "not_me" => {} _ => {} } }
fn h() {
    let res = match payload.cmd.as_str() {
        // "commented_out" => nope,
        "one" => {
            let v = args.get("body_key").unwrap_or("dflt");
            match v { "nested" => 1, _ => 2 }
        }
        "two" | "three" => RpcResponse::success(json!({ "k": "v" })),
        "four" => match scope(&a) {
            Ok(S::W) => x("five_is_body"),
            Err(e) => y(e),
        },
        cmd @ ("six" | "seven"
        | "eight") => RpcResponse::error(format!("{cmd} {}", c(')', '"'))),
        "nine" => if a { b } else { c }
        "ten" => r#"raw"#.len(),
        other => RpcResponse::error(format!("Command '{other}' not implemented")),
    };
}
"##;
    let served = served_verbs(src);
    let got: Vec<&str> = served.iter().map(String::as_str).collect();
    assert_eq!(
        got,
        vec!["eight", "four", "nine", "one", "seven", "six", "ten", "three", "two"]
    );
}

#[test]
fn tauri_parser_matches_the_acl_script_shape() {
    let src = r#"
        .invoke_handler(tauri::generate_handler![
            commands::a::first_cmd,
            // commands::a::commented_cmd,
            #[cfg(debug_assertions)]
            commands::b::debug_cmd,
            plain_cmd, // trailing comment
            Not::A::Command,
        ])
    "#;
    assert_eq!(
        tauri_commands(src),
        vec!["first_cmd", "debug_cmd", "plain_cmd"]
    );

    // The registry shape: the list is a fn's tail expression, so it closes
    // with a bare `]` (no `)`), and a `])` further down must not extend it.
    let src = r#"
        pub(crate) fn handler() -> impl Fn(Invoke<Wry>) -> bool {
            tauri::generate_handler![
                // ── section
                a::first_cmd,
                #[cfg(debug_assertions)]
                b::debug_cmd,
                // ── empty section stub
            ]
        }
        fn later() { let _ = vec![x(1, [2])]; }
        not_in_list,
    "#;
    assert_eq!(tauri_commands(src), vec!["first_cmd", "debug_cmd"]);
}
