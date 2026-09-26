//! G-ACTIONS validator (`plans/shell-ux-rearchitecture/drafts/actions-schema.md`,
//! FROZEN Round 39) for `actions.json` and `keybindings.json`.
//!
//! The one validator every writer goes through — the Actions/Keys tabs, the
//! WP-61 import and the WP-62 `iyke` routes — and the one the read path uses
//! to decide whether a file on disk is in force. It returns
//! `{ errors, warnings }` with G-ACTIONS §1.6 codes; any error refuses the
//! write whole and aborts the read for that file (the file is never wiped).
//!
//! What this layer can decide from the two files alone it decides here. What
//! needs the merged model (every built-in id, package actions, negative-rule
//! matches, the Lucide icon list) is WP-52's / the frontend's: unknown ids
//! are only warned when they fit no id namespace or name an undefined user
//! action, and `W_NEGATIVE_NOOP` is not raised here.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::settings::SettingsScope;

pub const ACTIONS_SCHEMA: &str = "urn:ikenga:actions:v1";
pub const KEYBINDINGS_SCHEMA: &str = "urn:ikenga:keybindings:v1";
pub const SCHEMA_VERSION: u64 = 1;

/// `when` limits (§4.1).
const WHEN_MAX_CHARS: usize = 512;
const WHEN_MAX_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Actions,
    Keybindings,
}

impl FileKind {
    pub const ALL: [FileKind; 2] = [FileKind::Actions, FileKind::Keybindings];

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "actions" => Ok(Self::Actions),
            "keybindings" => Ok(Self::Keybindings),
            _ => Err(format!("unknown actions file kind: {value}")),
        }
    }

    pub fn file_name(self) -> &'static str {
        match self {
            Self::Actions => "actions.json",
            Self::Keybindings => "keybindings.json",
        }
    }

    pub fn from_file_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.file_name() == name)
    }

    pub fn schema(self) -> &'static str {
        match self {
            Self::Actions => ACTIONS_SCHEMA,
            Self::Keybindings => KEYBINDINGS_SCHEMA,
        }
    }

    /// The array a file carries (`actions` / `bindings`).
    pub fn list_key(self) -> &'static str {
        match self {
            Self::Actions => "actions",
            Self::Keybindings => "bindings",
        }
    }

    /// An empty, valid document — what `actions_open_file` creates.
    pub fn skeleton(self) -> Value {
        let mut object = Map::new();
        object.insert("$schema".into(), Value::String(self.schema().into()));
        object.insert("version".into(), Value::from(SCHEMA_VERSION));
        object.insert(self.list_key().into(), Value::Array(Vec::new()));
        Value::Object(object)
    }
}

/// One validation finding. `path` is a JSON pointer into the document.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Issue {
    pub code: &'static str,
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Validation {
    pub errors: Vec<Issue>,
    pub warnings: Vec<Issue>,
}

impl Validation {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    fn error(&mut self, code: &'static str, path: impl Into<String>, message: impl Into<String>) {
        self.errors.push(Issue {
            code,
            path: path.into(),
            message: message.into(),
        });
    }

    fn warn(&mut self, code: &'static str, path: impl Into<String>, message: impl Into<String>) {
        self.warnings.push(Issue {
            code,
            path: path.into(),
            message: message.into(),
        });
    }

    /// A one-line summary of the errors, for `Err(String)` surfaces.
    pub fn summary(&self) -> String {
        self.errors
            .iter()
            .map(|issue| format!("{} at {}: {}", issue.code, pointer_or_root(&issue.path), issue.message))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

fn pointer_or_root(path: &str) -> &str {
    if path.is_empty() {
        "/"
    } else {
        path
    }
}

fn pointer(base: &str, segment: impl std::fmt::Display) -> String {
    let segment = segment.to_string().replace('~', "~0").replace('/', "~1");
    format!("{base}/{segment}")
}

// ---------------------------------------------------------------------------
// Frozen vocabularies (G-ACTIONS §1.3, §4.3, §8, §9.2, §10)
// ---------------------------------------------------------------------------

/// §10.3: the grandfathered bare built-in ids. Closed — never grows.
pub const BARE_BUILTIN_IDS: [&str; 35] = [
    "open",
    "open-to-side",
    "open-below",
    "open-loupe",
    "open-studio",
    "open-in-studio",
    "compare",
    "pin-sidebar",
    "pin-rail",
    "open-terminal-here",
    "open-terminal-side",
    "open-terminal-below",
    "hand-to-chi",
    "copy-path",
    "copy-name",
    "copy-uri",
    "reveal-files",
    "reveal-file-manager",
    "new-file",
    "new-folder",
    "rename",
    "delete",
    "make-dispatch",
    "kill-session",
    "run-now",
    "pause-resume",
    "open-last-log",
    "open-definition",
    "open-in-ngwa",
    "open-detail",
    "change-scope",
    "disable",
    "uninstall",
    "toggle-done",
    "open-source",
];

/// §9.2: reorderable, never hidden.
pub const LOCKED_IDS: [&str; 4] = ["delete", "pane.close", "tab.close", "kill-session"];

/// §4.6: commands fired by their owning widget, never by the frame
/// dispatcher, and never OS-wide.
const HOSTED_COMMANDS: [&str; 3] = ["companion.send", "companion.new-run", "companion.persistent-run"];

/// §4.3: the frozen context-key vocabulary (DEC-62 + the freeze additions).
const CONTEXT_KEYS: [&str; 14] = [
    "inputFocus",
    "terminalFocus",
    "explorerFocus",
    "filesFocus",
    "paneFocus",
    "paneKind",
    "resource",
    "resourceExtname",
    "project",
    "sessionFocus",
    "ngwaItemFocus",
    "ngwaItemKind",
    "dispatchFocus",
    "paletteOpen",
];

/// §1.3: keys undefined in a menu context.
const FOCUS_KEYS: [&str; 9] = [
    "inputFocus",
    "terminalFocus",
    "explorerFocus",
    "filesFocus",
    "paneFocus",
    "sessionFocus",
    "ngwaItemFocus",
    "dispatchFocus",
    "paletteOpen",
];

/// §1.3 fixed menu ids (plus the parameterized `section/<id>`, `native/<top>`).
const MENU_IDS: [&str; 18] = [
    "files",
    "files-view",
    "artifacts",
    "session",
    "automations",
    "ngwa-project",
    "scratchpads",
    "todos",
    "views",
    "tab",
    "address",
    "pane",
    "viewer-frame",
    "status",
    "rail",
    "rail-section",
    "rail-ngwa",
    "palette",
];

const NATIVE_MENUS: [&str; 9] = [
    "ikenga", "file", "edit", "view", "project", "chi", "ngwa", "window", "help",
];

/// §8.2: the six run variables.
pub const RUN_VARIABLES: [&str; 6] = [
    "file.path",
    "file.name",
    "selection",
    "project.root",
    "pane.url",
    "branch",
];

/// §8.1 run kinds. The last four are the DEC-55 trust-gated ones.
pub const RUN_KINDS: [&str; 6] = ["chi", "open", "shell", "iyke", "skill", "workflow"];
pub const GATED_RUN_KINDS: [&str; 4] = ["shell", "iyke", "skill", "workflow"];

const SEPARATOR: &str = "---";

// ---------------------------------------------------------------------------
// Id namespaces (§10.1)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdClass {
    /// `^[a-z0-9][a-z0-9-]{0,63}$`, not a bare built-in.
    User,
    /// One of the 35 grandfathered bare ids.
    BareBuiltin,
    /// `^[a-z][a-z0-9-]*(\.[a-z0-9-]+)+$`.
    DottedBuiltin,
    /// `${pkg_id}:${id}` — the only class containing `:`.
    Package,
    Invalid,
}

fn is_lower_digit_dash(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
}

fn is_user_grammar(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && id.len() <= 64
        && chars.all(is_lower_digit_dash)
}

fn is_dotted_grammar(id: &str) -> bool {
    let mut parts = id.split('.');
    let Some(head) = parts.next() else {
        return false;
    };
    let mut head_chars = head.chars();
    let head_ok = head_chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && head_chars.all(is_lower_digit_dash);
    let mut tail_count = 0;
    let tail_ok = parts.all(|part| {
        tail_count += 1;
        !part.is_empty() && part.chars().all(is_lower_digit_dash)
    });
    head_ok && tail_ok && tail_count > 0
}

pub fn classify_id(id: &str) -> IdClass {
    if let Some((pkg, local)) = id.split_once(':') {
        let ok = !pkg.is_empty()
            && !local.is_empty()
            && !local.contains(':')
            && !pkg.chars().any(char::is_whitespace)
            && !local.chars().any(char::is_whitespace);
        return if ok { IdClass::Package } else { IdClass::Invalid };
    }
    if is_dotted_grammar(id) {
        return IdClass::DottedBuiltin;
    }
    if is_user_grammar(id) {
        return if BARE_BUILTIN_IDS.contains(&id) {
            IdClass::BareBuiltin
        } else {
            IdClass::User
        };
    }
    IdClass::Invalid
}

/// §4.6: hosted commands (`terminal.*` and the three dispatch-input keys).
pub fn is_hosted_command(id: &str) -> bool {
    id.starts_with("terminal.") || HOSTED_COMMANDS.contains(&id)
}

pub fn is_known_menu_id(id: &str) -> bool {
    if MENU_IDS.contains(&id) {
        return true;
    }
    if let Some(section) = id.strip_prefix("section/") {
        return !section.is_empty() && !section.contains('/');
    }
    if let Some(top) = id.strip_prefix("native/") {
        return NATIVE_MENUS.contains(&top);
    }
    false
}

// ---------------------------------------------------------------------------
// Key grammar (§3.1)
// ---------------------------------------------------------------------------

const MODIFIERS: [&str; 5] = ["mod", "ctrl", "meta", "alt", "shift"];
const PUNCT_KEYS: [&str; 12] = ["`", "-", "=", "[", "]", "\\", ";", "'", ",", ".", "/", "?"];
const NAMED_KEYS: [&str; 16] = [
    "enter",
    "escape",
    "tab",
    "space",
    "backspace",
    "delete",
    "insert",
    "home",
    "end",
    "pageup",
    "pagedown",
    "arrowup",
    "arrowdown",
    "arrowleft",
    "arrowright",
    "plus",
];

fn is_valid_key_name(key: &str) -> bool {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            return true;
        }
    }
    if PUNCT_KEYS.contains(&key) || NAMED_KEYS.contains(&key) {
        return true;
    }
    key.strip_prefix('f')
        .and_then(|n| n.parse::<u8>().ok().filter(|_| !n.starts_with('0')))
        .is_some_and(|n| (1..=24).contains(&n))
}

fn parse_stroke(stroke: &str) -> Result<(), String> {
    if stroke.is_empty() {
        return Err("empty stroke".into());
    }
    if stroke.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(format!("`{stroke}` must be lowercase"));
    }
    let parts: Vec<&str> = stroke.split('+').collect();
    let (key, modifiers) = parts.split_last().expect("split yields at least one part");
    if key.is_empty() {
        return Err(format!("`{stroke}` has no key (the + key is spelled `plus`)"));
    }
    let mut seen: Vec<&str> = Vec::new();
    for modifier in modifiers {
        if !MODIFIERS.contains(modifier) {
            return Err(format!("`{modifier}` is not a modifier in `{stroke}`"));
        }
        if seen.contains(modifier) {
            return Err(format!("`{modifier}` repeats in `{stroke}`"));
        }
        seen.push(*modifier);
    }
    if seen.contains(&"mod") && (seen.contains(&"ctrl") || seen.contains(&"meta")) {
        return Err(format!("`mod` cannot combine with `ctrl` or `meta` in `{stroke}`"));
    }
    if !is_valid_key_name(key) {
        return Err(format!("`{key}` is not a key name"));
    }
    Ok(())
}

/// Parses a §3.1 key sequence and returns its stroke count (1 or 2).
pub fn parse_key_sequence(value: &str) -> Result<usize, String> {
    let strokes: Vec<&str> = value.split(' ').collect();
    if strokes.len() > 2 {
        return Err("a key sequence has at most two strokes".into());
    }
    for stroke in &strokes {
        parse_stroke(stroke)?;
    }
    Ok(strokes.len())
}

// ---------------------------------------------------------------------------
// `when` syntax (§4.1). A checker, not the evaluator: the evaluator is
// WP-49's TS parser. This walks the same EBNF so a write the frontend would
// refuse to parse is refused here first, and it reports the context keys used.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Token {
    LParen,
    RParen,
    Not,
    And,
    Or,
    Eq,
    NotEq,
    Match,
    Ident(String),
    Str,
}

fn tokenize_when(input: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            ' ' | '\t' | '\n' | '\r' => i += 1,
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            '&' if next == Some('&') => {
                tokens.push(Token::And);
                i += 2;
            }
            '|' if next == Some('|') => {
                tokens.push(Token::Or);
                i += 2;
            }
            '=' if next == Some('=') => {
                tokens.push(Token::Eq);
                i += 2;
            }
            '=' if next == Some('~') => {
                tokens.push(Token::Match);
                i += 2;
            }
            '!' if next == Some('=') => {
                tokens.push(Token::NotEq);
                i += 2;
            }
            '!' => {
                tokens.push(Token::Not);
                i += 1;
            }
            '\'' | '"' => {
                let quote = c;
                i += 1;
                let mut closed = false;
                while i < chars.len() {
                    match chars[i] {
                        '\\' if i + 1 < chars.len() => i += 2,
                        ch if ch == quote => {
                            closed = true;
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                if !closed {
                    return Err("unterminated string".into());
                }
                tokens.push(Token::Str);
            }
            c if c.is_ascii_lowercase() => {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_alphanumeric() {
                    i += 1;
                }
                tokens.push(Token::Ident(chars[start..i].iter().collect()));
            }
            other => return Err(format!("unexpected `{other}`")),
        }
    }
    Ok(tokens)
}

struct WhenParser<'a> {
    tokens: &'a [Token],
    pos: usize,
    keys: Vec<String>,
}

impl WhenParser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn bump(&mut self) -> Option<&Token> {
        let token = self.tokens.get(self.pos);
        self.pos += 1;
        token
    }

    fn or_expr(&mut self, depth: usize) -> Result<(), String> {
        self.and_expr(depth)?;
        while self.peek() == Some(&Token::Or) {
            self.pos += 1;
            self.and_expr(depth)?;
        }
        Ok(())
    }

    fn and_expr(&mut self, depth: usize) -> Result<(), String> {
        self.unary(depth)?;
        while self.peek() == Some(&Token::And) {
            self.pos += 1;
            self.unary(depth)?;
        }
        Ok(())
    }

    fn unary(&mut self, depth: usize) -> Result<(), String> {
        if depth > WHEN_MAX_DEPTH {
            return Err(format!("nesting deeper than {WHEN_MAX_DEPTH}"));
        }
        if self.peek() == Some(&Token::Not) {
            self.pos += 1;
            return self.unary(depth + 1);
        }
        self.primary(depth)
    }

    fn primary(&mut self, depth: usize) -> Result<(), String> {
        match self.bump().cloned() {
            Some(Token::LParen) => {
                self.or_expr(depth + 1)?;
                match self.bump() {
                    Some(Token::RParen) => Ok(()),
                    _ => Err("missing `)`".into()),
                }
            }
            Some(Token::Ident(word)) => {
                match word.as_str() {
                    "always" | "true" | "false" => {
                        if matches!(self.peek(), Some(Token::Eq | Token::NotEq | Token::Match)) {
                            return Err(format!("`{word}` is a literal, not a context key"));
                        }
                        return Ok(());
                    }
                    "global" => {
                        return Err("`global` is only valid as a whole expression".into());
                    }
                    _ => {}
                }
                self.keys.push(word);
                match self.peek() {
                    Some(Token::Eq | Token::NotEq) => {
                        self.pos += 1;
                        match self.bump() {
                            Some(Token::Str) => Ok(()),
                            Some(Token::Ident(value)) if value == "true" || value == "false" => {
                                Ok(())
                            }
                            _ => Err("a comparison needs a quoted string, true or false".into()),
                        }
                    }
                    Some(Token::Match) => {
                        self.pos += 1;
                        match self.bump() {
                            Some(Token::Str) => Ok(()),
                            _ => Err("`=~` needs a quoted glob".into()),
                        }
                    }
                    _ => Ok(()),
                }
            }
            Some(other) => Err(format!("unexpected {other:?}")),
            None => Err("unexpected end of expression".into()),
        }
    }
}

/// Checks a `when` against §4.1 and returns the context keys it names.
/// Empty / whitespace is `always`; the three legacy words are accepted only
/// as a whole expression (§4.4).
pub fn check_when(input: &str) -> Result<Vec<String>, String> {
    if input.chars().count() > WHEN_MAX_CHARS {
        return Err(format!("longer than {WHEN_MAX_CHARS} characters"));
    }
    let trimmed = input.trim();
    match trimmed {
        "" | "global" => return Ok(Vec::new()),
        "not-input" => return Ok(vec!["inputFocus".into()]),
        "terminal-focus" => return Ok(vec!["terminalFocus".into()]),
        _ => {}
    }
    let tokens = tokenize_when(trimmed)?;
    let mut parser = WhenParser {
        tokens: &tokens,
        pos: 0,
        keys: Vec::new(),
    };
    parser.or_expr(0)?;
    if parser.pos != tokens.len() {
        return Err("unexpected trailing input".into());
    }
    Ok(parser.keys)
}

// ---------------------------------------------------------------------------
// Documents
// ---------------------------------------------------------------------------

/// Everything the validator needs to know beyond the document itself.
#[derive(Clone, Copy)]
pub struct ValidateContext<'a> {
    pub kind: FileKind,
    pub scope: SettingsScope,
    /// User action ids defined in the personal and project `actions.json`
    /// (for `W_UNKNOWN_COMMAND`). `None` skips that check.
    pub user_action_ids: Option<&'a HashSet<String>>,
}

/// Parses strict JSON (no comments, no trailing commas) and validates it.
/// Returns the document only when it parsed.
pub fn validate_bytes(ctx: ValidateContext<'_>, bytes: &[u8]) -> (Option<Value>, Validation) {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => {
            let validation = validate_document(ctx, &value);
            (Some(value), validation)
        }
        Err(error) => {
            let mut validation = Validation::default();
            validation.error("E_JSON", "", format!("not strict JSON: {error}"));
            (None, validation)
        }
    }
}

pub fn validate_document(ctx: ValidateContext<'_>, value: &Value) -> Validation {
    let mut v = Validation::default();
    let Some(object) = value.as_object() else {
        v.error("E_FIELD", "", "the document must be a JSON object");
        return v;
    };
    match object.get("version") {
        Some(Value::Number(n)) if n.as_u64() == Some(SCHEMA_VERSION) => {}
        Some(Value::Number(n)) if n.as_u64().is_some_and(|n| n > SCHEMA_VERSION) => v.error(
            "E_VERSION",
            "/version",
            format!("version {n} is newer than supported version {SCHEMA_VERSION}"),
        ),
        Some(_) => v.error("E_VERSION", "/version", "version must be 1"),
        None => v.error("E_VERSION", "/version", "version is missing"),
    }
    match object.get("$schema") {
        None => {}
        Some(Value::String(schema)) if schema == ctx.kind.schema() => {}
        Some(_) => v.error(
            "E_FIELD",
            "/$schema",
            format!("$schema must be \"{}\"", ctx.kind.schema()),
        ),
    }
    match ctx.kind {
        FileKind::Actions => validate_actions_body(ctx, object, &mut v),
        FileKind::Keybindings => validate_keybindings_body(ctx, object, &mut v),
    }
    v
}

fn warn_unknown_keys(object: &Map<String, Value>, known: &[&str], base: &str, v: &mut Validation) {
    for key in object.keys() {
        if !known.contains(&key.as_str()) {
            v.warn(
                "W_UNKNOWN_FIELD",
                pointer(base, key),
                format!("unknown field `{key}` (kept, ignored)"),
            );
        }
    }
}

fn check_when_field(
    when: &str,
    path: &str,
    placement: bool,
    v: &mut Validation,
) {
    match check_when(when) {
        Err(message) => v.error("E_WHEN_SYNTAX", path, message),
        Ok(keys) => {
            let mut reported: Vec<&str> = Vec::new();
            for key in &keys {
                if reported.contains(&key.as_str()) {
                    continue;
                }
                reported.push(key.as_str());
                if placement && FOCUS_KEYS.contains(&key.as_str()) {
                    v.warn(
                        "W_FOCUS_IN_PLACEMENT",
                        path,
                        format!("`{key}` is undefined in a menu context; the item never shows"),
                    );
                } else if !CONTEXT_KEYS.contains(&key.as_str()) {
                    v.warn(
                        "W_UNKNOWN_CONTEXT_KEY",
                        path,
                        format!("`{key}` is not a context key (evaluates undefined)"),
                    );
                }
            }
        }
    }
}

fn is_kebab_icon(icon: &str) -> bool {
    !icon.is_empty()
        && icon.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

// --- actions.json -----------------------------------------------------------

fn validate_actions_body(ctx: ValidateContext<'_>, object: &Map<String, Value>, v: &mut Validation) {
    warn_unknown_keys(object, &["$schema", "version", "actions", "menus"], "", v);
    match object.get("actions") {
        None => {}
        Some(Value::Array(actions)) => {
            let mut seen: HashSet<&str> = HashSet::new();
            for (index, action) in actions.iter().enumerate() {
                let path = pointer("/actions", index);
                if let Some(id) = validate_action(ctx, action, &path, v) {
                    if !seen.insert(id) {
                        v.error(
                            "E_ID_DUP",
                            pointer(&path, "id"),
                            format!("duplicate action id `{id}` in this file"),
                        );
                    }
                }
            }
        }
        Some(_) => v.error("E_FIELD", "/actions", "actions must be an array"),
    }
    match object.get("menus") {
        None => {}
        Some(Value::Object(menus)) => {
            for (menu_id, menu) in menus {
                validate_menu_override(ctx, menu_id, menu, &pointer("/menus", menu_id), v);
            }
        }
        Some(_) => v.error("E_FIELD", "/menus", "menus must be an object"),
    }
}

/// Validates one action; returns its id when it is a string.
fn validate_action<'a>(
    ctx: ValidateContext<'_>,
    action: &'a Value,
    path: &str,
    v: &mut Validation,
) -> Option<&'a str> {
    let Some(object) = action.as_object() else {
        v.error("E_FIELD", path, "an action must be an object");
        return None;
    };
    warn_unknown_keys(
        object,
        &["id", "name", "icon", "description", "run", "placements", "scope"],
        path,
        v,
    );

    let id = match object.get("id") {
        Some(Value::String(id)) => {
            let id_path = pointer(path, "id");
            match classify_id(id) {
                IdClass::User => {}
                IdClass::BareBuiltin | IdClass::DottedBuiltin => v.error(
                    "E_ID_BUILTIN",
                    id_path,
                    format!("`{id}` is a built-in id; user actions cannot redefine built-ins"),
                ),
                IdClass::Package => v.error(
                    "E_ID_BUILTIN",
                    id_path,
                    format!("`{id}` is a package id; user action ids have no `:`"),
                ),
                IdClass::Invalid => v.error(
                    "E_ID_GRAMMAR",
                    id_path,
                    format!("`{id}` must match ^[a-z0-9][a-z0-9-]{{0,63}}$"),
                ),
            }
            Some(id.as_str())
        }
        Some(_) => {
            v.error("E_FIELD", pointer(path, "id"), "id must be a string");
            None
        }
        None => {
            v.error("E_FIELD", pointer(path, "id"), "id is required");
            None
        }
    };

    match object.get("name") {
        Some(Value::String(name)) if (1..=80).contains(&name.chars().count()) => {}
        Some(_) => v.error(
            "E_FIELD",
            pointer(path, "name"),
            "name must be a string of 1–80 characters",
        ),
        None => v.error("E_FIELD", pointer(path, "name"), "name is required"),
    }

    match object.get("icon") {
        None => {}
        Some(Value::String(icon)) => {
            if !is_kebab_icon(icon) {
                v.warn(
                    "W_UNKNOWN_ICON",
                    pointer(path, "icon"),
                    format!("`{icon}` is not a Lucide icon name (kept; renders `zap`)"),
                );
            }
        }
        Some(_) => v.error("E_FIELD", pointer(path, "icon"), "icon must be a string"),
    }

    match object.get("description") {
        None | Some(Value::String(_)) => {}
        Some(_) => v.error(
            "E_FIELD",
            pointer(path, "description"),
            "description must be a string",
        ),
    }

    match object.get("run") {
        Some(run) => validate_run(run, &pointer(path, "run"), v),
        None => v.error("E_RUN_KIND", pointer(path, "run"), "run is required"),
    }

    match object.get("placements") {
        None => {}
        Some(Value::Array(placements)) => {
            for (index, placement) in placements.iter().enumerate() {
                validate_placement(placement, &pointer(&pointer(path, "placements"), index), v);
            }
        }
        Some(_) => v.error(
            "E_FIELD",
            pointer(path, "placements"),
            "placements must be an array",
        ),
    }

    let scope_path = pointer(path, "scope");
    match object.get("scope").and_then(Value::as_str) {
        Some(scope @ ("personal" | "project")) => {
            let expected = scope_name(ctx.scope);
            if scope != expected {
                v.error(
                    "E_SCOPE_MISMATCH",
                    scope_path,
                    format!("scope is `{scope}` but the action sits in the {expected} file"),
                );
            }
        }
        Some(other) => v.error(
            "E_FIELD",
            scope_path,
            format!("scope `{other}` must be \"personal\" or \"project\""),
        ),
        None => v.error(
            "E_FIELD",
            scope_path,
            "scope is required (\"personal\" or \"project\")",
        ),
    }
    id
}

pub fn scope_name(scope: SettingsScope) -> &'static str {
    match scope {
        SettingsScope::Personal => "personal",
        SettingsScope::Project => "project",
    }
}

fn validate_placement(placement: &Value, path: &str, v: &mut Validation) {
    let Some(object) = placement.as_object() else {
        v.error("E_FIELD", path, "a placement must be an object");
        return;
    };
    warn_unknown_keys(object, &["at", "when"], path, v);
    match object.get("at") {
        Some(Value::String(at)) => {
            if !is_known_menu_id(at) {
                v.warn(
                    "W_UNKNOWN_MENU",
                    pointer(path, "at"),
                    format!("`{at}` is not a menu id (kept, inert)"),
                );
            }
        }
        Some(_) => v.error("E_FIELD", pointer(path, "at"), "at must be a string"),
        None => v.error("E_FIELD", pointer(path, "at"), "at is required"),
    }
    match object.get("when") {
        None => {}
        Some(Value::String(when)) => check_when_field(when, &pointer(path, "when"), true, v),
        Some(_) => v.error("E_FIELD", pointer(path, "when"), "when must be a string"),
    }
}

fn validate_menu_override(
    ctx: ValidateContext<'_>,
    menu_id: &str,
    menu: &Value,
    path: &str,
    v: &mut Validation,
) {
    if !is_known_menu_id(menu_id) {
        v.warn(
            "W_UNKNOWN_MENU",
            path,
            format!("`{menu_id}` is not a menu id (kept, inert)"),
        );
    }
    let Some(object) = menu.as_object() else {
        v.error("E_FIELD", path, "a menu override must be an object");
        return;
    };
    warn_unknown_keys(object, &["items", "hidden"], path, v);
    for field in ["items", "hidden"] {
        let field_path = pointer(path, field);
        match object.get(field) {
            None => {}
            Some(Value::Array(ids)) => {
                for (index, entry) in ids.iter().enumerate() {
                    let entry_path = pointer(&field_path, index);
                    let Some(id) = entry.as_str() else {
                        v.error("E_FIELD", entry_path, "menu entries must be strings");
                        continue;
                    };
                    if id == SEPARATOR {
                        if field == "hidden" {
                            v.error("E_FIELD", entry_path, "`---` is a separator, not an id");
                        }
                        continue;
                    }
                    if field == "hidden" && LOCKED_IDS.contains(&id) {
                        v.error(
                            "E_LOCKED_HIDDEN",
                            entry_path.clone(),
                            format!("`{id}` is locked: it can be reordered, never hidden"),
                        );
                    }
                    check_command_id(ctx, id, &entry_path, v);
                }
            }
            Some(_) => v.error("E_FIELD", field_path, format!("{field} must be an array")),
        }
    }
}

/// Warns when an id fits no namespace or names an undefined user action.
fn check_command_id(ctx: ValidateContext<'_>, id: &str, path: &str, v: &mut Validation) {
    match classify_id(id) {
        IdClass::Invalid => v.warn(
            "W_UNKNOWN_COMMAND",
            path,
            format!("`{id}` is not an action id (kept, inert)"),
        ),
        IdClass::User => {
            if let Some(known) = ctx.user_action_ids {
                if !known.contains(id) {
                    v.warn(
                        "W_UNKNOWN_COMMAND",
                        path,
                        format!("no action defines `{id}` (kept, inert)"),
                    );
                }
            }
        }
        IdClass::BareBuiltin | IdClass::DottedBuiltin | IdClass::Package => {}
    }
}

// --- run (§8.1, §8.2) -------------------------------------------------------

fn validate_run(run: &Value, path: &str, v: &mut Validation) {
    let Some(object) = run.as_object() else {
        v.error("E_RUN_KIND", path, "run must be an object");
        return;
    };
    let kind = match object.get("kind").and_then(Value::as_str) {
        Some(kind) if RUN_KINDS.contains(&kind) => kind,
        Some(kind) => {
            v.error(
                "E_RUN_KIND",
                pointer(path, "kind"),
                format!("unknown run kind `{kind}`"),
            );
            return;
        }
        None => {
            v.error("E_RUN_KIND", pointer(path, "kind"), "run.kind is required");
            return;
        }
    };
    let required_string = |field: &str, v: &mut Validation| match object.get(field) {
        Some(Value::String(value)) if !value.trim().is_empty() => {}
        _ => v.error(
            "E_RUN_KIND",
            pointer(path, field),
            format!("a `{kind}` run needs a non-empty string `{field}`"),
        ),
    };
    let optional_string = |field: &str, v: &mut Validation| match object.get(field) {
        None | Some(Value::String(_)) => {}
        Some(_) => v.error("E_FIELD", pointer(path, field), format!("{field} must be a string")),
    };
    let known: &[&str] = match kind {
        "chi" => {
            match object.get("target").and_then(Value::as_str) {
                Some("active" | "new") => {
                    if object.contains_key("engineId") {
                        v.error(
                            "E_RUN_KIND",
                            pointer(path, "engineId"),
                            "engineId is only valid with target \"engine\"",
                        );
                    }
                }
                Some("engine") => required_string("engineId", v),
                _ => v.error(
                    "E_RUN_KIND",
                    pointer(path, "target"),
                    "a `chi` run needs target \"active\", \"new\" or \"engine\"",
                ),
            }
            match object.get("prompt") {
                Some(Value::String(_)) => {}
                _ => v.error(
                    "E_RUN_KIND",
                    pointer(path, "prompt"),
                    "a `chi` run needs a string `prompt`",
                ),
            }
            &["kind", "target", "engineId", "prompt"]
        }
        "shell" => {
            required_string("command", v);
            optional_string("cwd", v);
            match object.get("confirm") {
                None | Some(Value::Bool(_)) => {}
                Some(_) => v.error("E_FIELD", pointer(path, "confirm"), "confirm must be a boolean"),
            }
            &["kind", "command", "cwd", "confirm"]
        }
        "iyke" => {
            required_string("route", v);
            match object.get("method") {
                None => {}
                Some(Value::String(method)) if method == "GET" || method == "POST" => {}
                Some(_) => v.error(
                    "E_FIELD",
                    pointer(path, "method"),
                    "method must be \"GET\" or \"POST\"",
                ),
            }
            &["kind", "route", "method"]
        }
        "skill" => {
            required_string("skill", v);
            &["kind", "skill"]
        }
        "workflow" => {
            required_string("workflow", v);
            &["kind", "workflow"]
        }
        _ => {
            required_string("url", v);
            &["kind", "url"]
        }
    };
    warn_unknown_keys(object, known, path, v);
    for (field, value) in object {
        if field == "kind" {
            continue;
        }
        if let Some(text) = value.as_str() {
            for name in template_variables(text) {
                if !RUN_VARIABLES.contains(&name.as_str()) {
                    v.error(
                        "E_VAR_UNKNOWN",
                        pointer(path, field),
                        format!(
                            "`{{{{{name}}}}}` is not a run variable ({})",
                            RUN_VARIABLES.join(", ")
                        ),
                    );
                }
            }
        }
    }
}

/// Every `{{name}}` in a template, `name` verbatim (spaces included, so
/// `{{ file.path }}` is reported rather than silently not interpolated).
pub fn template_variables(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        names.push(after[..end].to_string());
        rest = &after[end + 2..];
    }
    names
}

// --- keybindings.json (§1.5, §6) ----------------------------------------------

fn validate_keybindings_body(
    ctx: ValidateContext<'_>,
    object: &Map<String, Value>,
    v: &mut Validation,
) {
    warn_unknown_keys(object, &["$schema", "version", "bindings"], "", v);
    match object.get("bindings") {
        None => {}
        Some(Value::Array(rules)) => {
            for (index, rule) in rules.iter().enumerate() {
                validate_rule(ctx, rule, &pointer("/bindings", index), v);
            }
        }
        Some(_) => v.error("E_FIELD", "/bindings", "bindings must be an array"),
    }
}

fn validate_rule(ctx: ValidateContext<'_>, rule: &Value, path: &str, v: &mut Validation) {
    let Some(object) = rule.as_object() else {
        v.error("E_FIELD", path, "a binding must be an object");
        return;
    };
    warn_unknown_keys(object, &["key", "command", "when", "scope", "platform"], path, v);

    let strokes = match object.get("key") {
        Some(Value::String(key)) => match parse_key_sequence(key) {
            Ok(strokes) => Some(strokes),
            Err(message) => {
                v.error("E_KEY_GRAMMAR", pointer(path, "key"), message);
                None
            }
        },
        Some(_) => {
            v.error("E_FIELD", pointer(path, "key"), "key must be a string");
            None
        }
        None => {
            v.error("E_FIELD", pointer(path, "key"), "key is required");
            None
        }
    };

    let command_path = pointer(path, "command");
    let command = match object.get("command") {
        Some(Value::String(command)) => {
            let id = command.strip_prefix('-').unwrap_or(command);
            if id.is_empty() {
                v.error("E_FIELD", command_path.clone(), "command is empty");
                None
            } else if id.starts_with("action:") {
                v.error(
                    "E_FIELD",
                    command_path.clone(),
                    format!("`{command}`: the `action:` prefix is not valid in files; use the id alone"),
                );
                None
            } else {
                check_command_id(ctx, id, &command_path, v);
                Some(id)
            }
        }
        Some(_) => {
            v.error("E_FIELD", command_path.clone(), "command must be a string");
            None
        }
        None => {
            v.error("E_FIELD", command_path.clone(), "command is required");
            None
        }
    };

    let when = match object.get("when") {
        None => None,
        Some(Value::String(when)) => {
            check_when_field(when, &pointer(path, "when"), false, v);
            Some(when.as_str())
        }
        Some(_) => {
            v.error("E_FIELD", pointer(path, "when"), "when must be a string");
            None
        }
    };

    match object.get("platform") {
        None => {}
        Some(Value::String(platform)) if platform == "mac" || platform == "other" => {}
        Some(_) => v.error(
            "E_FIELD",
            pointer(path, "platform"),
            "platform must be \"mac\" or \"other\"",
        ),
    }

    let scope_path = pointer(path, "scope");
    let os = match object.get("scope") {
        None => false,
        Some(Value::String(scope)) if scope == "app" => false,
        Some(Value::String(scope)) if scope == "os" => true,
        Some(_) => {
            v.error("E_FIELD", scope_path.clone(), "scope must be \"app\" or \"os\"");
            false
        }
    };

    if os {
        // DEC-60: only the shell (defaults) and the user (personal file) bind
        // OS-wide keys.
        if ctx.scope != SettingsScope::Personal {
            v.error(
                "E_OS_LAYER",
                scope_path.clone(),
                "scope \"os\" is only valid in the personal keybindings.json",
            );
        }
        if let Some(id) = command {
            if classify_id(id) == IdClass::Package {
                v.error(
                    "E_OS_COMMAND",
                    command_path.clone(),
                    format!("`{id}` is a package action; packages never get an OS-wide key"),
                );
            } else if is_hosted_command(id) {
                v.error(
                    "E_OS_COMMAND",
                    command_path.clone(),
                    format!("`{id}` is hosted by a widget that has no focus while Ikenga is unfocused"),
                );
            }
        }
        if when.is_some_and(|when| !when.trim().is_empty()) {
            v.error(
                "E_OS_WHEN",
                pointer(path, "when"),
                "an OS-wide rule ignores focus and cannot carry a when",
            );
        }
        if strokes == Some(2) {
            v.error(
                "E_OS_CHORD",
                pointer(path, "key"),
                "an OS-wide rule is a single stroke",
            );
        }
    } else if let Some(id) = command.filter(|id| id.starts_with("os.")) {
        v.error(
            "E_OS_COMMAND",
            command_path,
            format!("`{id}` is OS-only: bind it with \"scope\": \"os\" in the personal file"),
        );
    }
}

/// The user action ids a parsed `actions.json` defines.
pub fn defined_action_ids(document: &Value) -> impl Iterator<Item = &str> {
    document
        .get("actions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|action| action.get("id").and_then(Value::as_str))
}

/// The bytes a validated document is written as: `$schema` filled in when
/// absent, every other field (unknown ones included) preserved, pretty JSON
/// with a trailing newline.
pub fn document_bytes(kind: FileKind, document: &Value) -> Result<Vec<u8>, String> {
    let mut document = document.clone();
    if let Some(object) = document.as_object_mut() {
        object
            .entry("$schema")
            .or_insert_with(|| Value::String(kind.schema().into()));
    }
    let mut bytes = serde_json::to_vec_pretty(&document)
        .map_err(|e| format!("serialize {}: {e}", kind.file_name()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx(kind: FileKind, scope: SettingsScope) -> ValidateContext<'static> {
        ValidateContext {
            kind,
            scope,
            user_action_ids: None,
        }
    }

    fn codes(issues: &[Issue]) -> Vec<&'static str> {
        issues.iter().map(|issue| issue.code).collect()
    }

    /// The §1.2 example, split by scope.
    fn personal_actions() -> Value {
        json!({
            "$schema": "urn:ikenga:actions:v1",
            "version": 1,
            "actions": [{
                "id": "explain-file",
                "name": "Explain this file",
                "icon": "sparkles",
                "description": "Sends a prompt template to the active session.",
                "run": { "kind": "chi", "target": "active", "prompt": "Explain {{file.path}}" },
                "placements": [ { "at": "files", "when": "resource =~ '*.{ts,rs}'" } ],
                "scope": "personal"
            }],
            "menus": {
                "files": { "items": ["open", "open-to-side", "---", "explain-file"], "hidden": ["copy-name"] }
            }
        })
    }

    fn project_actions() -> Value {
        json!({
            "$schema": "urn:ikenga:actions:v1",
            "version": 1,
            "actions": [{
                "id": "refresh-pulse",
                "name": "Refresh pulse snapshots",
                "icon": "refresh-cw",
                "run": { "kind": "shell", "command": "scripts/pulse/build-all.sh", "cwd": "{{project.root}}", "confirm": true },
                "placements": [ { "at": "palette" }, { "at": "section/automations" } ],
                "scope": "project"
            }],
            "menus": { "section/automations": { "items": ["refresh-pulse", "---", "section.collapse-others"] } }
        })
    }

    /// The §1.5 example (personal: it carries OS rules).
    fn personal_bindings() -> Value {
        json!({
            "$schema": "urn:ikenga:keybindings:v1",
            "version": 1,
            "bindings": [
                { "key": "mod+shift+e", "command": "explain-file", "when": "filesFocus && resource =~ '*.{ts,rs}'" },
                { "key": "mod+k mod+r", "command": "release-status" },
                { "key": "mod+w", "command": "-pane.close" },
                { "key": "mod+alt+w", "command": "pane.close" },
                { "key": "alt+space", "command": "-os.summon", "scope": "os", "platform": "mac" },
                { "key": "ctrl+alt+space", "command": "os.summon", "scope": "os", "platform": "mac" }
            ]
        })
    }

    #[test]
    fn spec_examples_validate_clean() {
        let v = validate_document(ctx(FileKind::Actions, SettingsScope::Personal), &personal_actions());
        assert!(v.is_ok(), "{:?}", v.errors);
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);
        let v = validate_document(ctx(FileKind::Actions, SettingsScope::Project), &project_actions());
        assert!(v.is_ok(), "{:?}", v.errors);
        let v = validate_document(ctx(FileKind::Keybindings, SettingsScope::Personal), &personal_bindings());
        assert!(v.is_ok(), "{:?}", v.errors);
    }

    #[test]
    fn both_files_round_trip_through_the_schema() {
        for (kind, scope, document) in [
            (FileKind::Actions, SettingsScope::Personal, personal_actions()),
            (FileKind::Actions, SettingsScope::Project, project_actions()),
            (FileKind::Keybindings, SettingsScope::Personal, personal_bindings()),
        ] {
            let bytes = document_bytes(kind, &document).unwrap();
            let (parsed, v) = validate_bytes(ctx(kind, scope), &bytes);
            assert!(v.is_ok(), "{:?}", v.errors);
            assert_eq!(parsed.unwrap(), document);
        }
    }

    #[test]
    fn unknown_fields_are_preserved_and_warned() {
        let mut document = personal_actions();
        document["futureTop"] = json!({ "x": 1 });
        document["actions"][0]["futureField"] = json!(true);
        let v = validate_document(ctx(FileKind::Actions, SettingsScope::Personal), &document);
        assert!(v.is_ok());
        assert_eq!(codes(&v.warnings), vec!["W_UNKNOWN_FIELD", "W_UNKNOWN_FIELD"]);
        let bytes = document_bytes(FileKind::Actions, &document).unwrap();
        let reparsed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(reparsed["futureTop"], json!({ "x": 1 }));
        assert_eq!(reparsed["actions"][0]["futureField"], json!(true));
    }

    #[test]
    fn strict_json_and_version_are_enforced() {
        let c = ctx(FileKind::Actions, SettingsScope::Personal);
        let (doc, v) = validate_bytes(c, br#"{"version":1,}"#);
        assert!(doc.is_none());
        assert_eq!(codes(&v.errors), vec!["E_JSON"]);
        let (_, v) = validate_bytes(c, b"// comment\n{\"version\":1}");
        assert_eq!(codes(&v.errors), vec!["E_JSON"]);
        let (_, v) = validate_bytes(c, br#"{"version":2}"#);
        assert_eq!(codes(&v.errors), vec!["E_VERSION"]);
        let (_, v) = validate_bytes(c, br#"{"actions":[]}"#);
        assert_eq!(codes(&v.errors), vec!["E_VERSION"]);
        let (_, v) = validate_bytes(c, br#"{"$schema":"urn:ikenga:settings:v1","version":1}"#);
        assert_eq!(codes(&v.errors), vec!["E_FIELD"]);
    }

    #[test]
    fn action_ids_follow_the_namespaces() {
        assert_eq!(classify_id("explain-file"), IdClass::User);
        assert_eq!(classify_id("delete"), IdClass::BareBuiltin);
        assert_eq!(classify_id("pane.close"), IdClass::DottedBuiltin);
        assert_eq!(classify_id("com.ikenga.git:stage-file"), IdClass::Package);
        assert_eq!(classify_id("Explain"), IdClass::Invalid);
        assert_eq!(classify_id("-lead"), IdClass::Invalid);
        let c = ctx(FileKind::Actions, SettingsScope::Personal);
        let mut document = personal_actions();
        document["actions"][0]["id"] = json!("delete");
        assert_eq!(codes(&validate_document(c, &document).errors), vec!["E_ID_BUILTIN"]);
        document["actions"][0]["id"] = json!("pane.close");
        assert_eq!(codes(&validate_document(c, &document).errors), vec!["E_ID_BUILTIN"]);
        document["actions"][0]["id"] = json!("Bad_Id");
        assert_eq!(codes(&validate_document(c, &document).errors), vec!["E_ID_GRAMMAR"]);
        document["actions"][0]["id"] = json!("explain-file");
        let dup = document["actions"][0].clone();
        document["actions"].as_array_mut().unwrap().push(dup);
        assert_eq!(codes(&validate_document(c, &document).errors), vec!["E_ID_DUP"]);
    }

    #[test]
    fn action_scope_must_match_the_file() {
        let v = validate_document(ctx(FileKind::Actions, SettingsScope::Project), &personal_actions());
        assert_eq!(codes(&v.errors), vec!["E_SCOPE_MISMATCH"]);
    }

    #[test]
    fn run_kinds_and_variables_are_checked() {
        let c = ctx(FileKind::Actions, SettingsScope::Personal);
        let mut document = personal_actions();
        document["actions"][0]["run"] = json!({ "kind": "chi.dispatch", "prompt": "x" });
        assert_eq!(codes(&validate_document(c, &document).errors), vec!["E_RUN_KIND"]);
        document["actions"][0]["run"] = json!({ "kind": "chi", "target": "engine", "prompt": "x" });
        assert_eq!(codes(&validate_document(c, &document).errors), vec!["E_RUN_KIND"]);
        document["actions"][0]["run"] = json!({ "kind": "shell" });
        assert_eq!(codes(&validate_document(c, &document).errors), vec!["E_RUN_KIND"]);
        document["actions"][0]["run"] = json!({ "kind": "open", "url": "https://x/{{file.name}}/{{cwd}}" });
        assert_eq!(codes(&validate_document(c, &document).errors), vec!["E_VAR_UNKNOWN"]);
        document["actions"][0]["run"] = json!({ "kind": "iyke", "route": "/pane/navigate", "method": "PUT" });
        assert_eq!(codes(&validate_document(c, &document).errors), vec!["E_FIELD"]);
        for run in [
            json!({ "kind": "skill", "skill": "release-status" }),
            json!({ "kind": "workflow", "workflow": "com.ikenga.tasks:nightly" }),
            json!({ "kind": "iyke", "route": "/pane/navigate" }),
            json!({ "kind": "chi", "target": "engine", "engineId": "gemini", "prompt": "{{selection}}" }),
        ] {
            document["actions"][0]["run"] = run;
            assert!(validate_document(c, &document).is_ok());
        }
    }

    #[test]
    fn locked_items_cannot_be_hidden() {
        let mut document = personal_actions();
        document["menus"]["files"]["hidden"] = json!(["delete"]);
        let v = validate_document(ctx(FileKind::Actions, SettingsScope::Personal), &document);
        assert_eq!(codes(&v.errors), vec!["E_LOCKED_HIDDEN"]);
        document["menus"]["tab"] = json!({ "items": ["tab.close", "---"], "hidden": ["tab.close-others"] });
        document["menus"]["files"]["hidden"] = json!([]);
        assert!(validate_document(ctx(FileKind::Actions, SettingsScope::Personal), &document).is_ok());
    }

    #[test]
    fn placements_warn_on_unknown_menus_and_focus_keys() {
        let mut document = personal_actions();
        document["actions"][0]["placements"] = json!([
            { "at": "section" },
            { "at": "native/view" },
            { "at": "files", "when": "filesFocus && resource =~ '*.ts'" }
        ]);
        let v = validate_document(ctx(FileKind::Actions, SettingsScope::Personal), &document);
        assert!(v.is_ok());
        assert_eq!(codes(&v.warnings), vec!["W_UNKNOWN_MENU", "W_FOCUS_IN_PLACEMENT"]);
    }

    #[test]
    fn project_os_rules_are_rejected() {
        let v = validate_document(ctx(FileKind::Keybindings, SettingsScope::Project), &personal_bindings());
        assert_eq!(codes(&v.errors), vec!["E_OS_LAYER", "E_OS_LAYER"]);
        assert!(v.errors.iter().all(|issue| issue.path.ends_with("/scope")));
    }

    #[test]
    fn os_rules_have_no_when_no_chord_and_no_package_or_hosted_command() {
        let c = ctx(FileKind::Keybindings, SettingsScope::Personal);
        let document = json!({ "version": 1, "bindings": [
            { "key": "ctrl+alt+k", "command": "com.ikenga.git:stage-file", "scope": "os" },
            { "key": "ctrl+alt+j", "command": "terminal.clear", "scope": "os" },
            { "key": "ctrl+alt+l", "command": "palette.open", "scope": "os", "when": "!inputFocus" },
            { "key": "ctrl+alt+m ctrl+alt+n", "command": "palette.open", "scope": "os" },
            { "key": "ctrl+alt+o", "command": "os.summon" },
            { "key": "ctrl+alt+p", "command": "explain-file", "scope": "os" }
        ]});
        let v = validate_document(c, &document);
        assert_eq!(
            codes(&v.errors),
            vec!["E_OS_COMMAND", "E_OS_COMMAND", "E_OS_WHEN", "E_OS_CHORD", "E_OS_COMMAND"]
        );
    }

    #[test]
    fn key_grammar() {
        for ok in ["mod+k", "mod+k mod+r", "shift+alt+f", "mod+shift+\\", "?", "mod+=", "f12", "plus", "ctrl+alt+shift+s", "alt+1"] {
            assert!(parse_key_sequence(ok).is_ok(), "{ok}");
        }
        for bad in ["Mod+K", "mod+ctrl+k", "mod+mod+k", "mod++", "mod+k  mod+r", "a b c", "f25", "f0", "hyper+k", "", "mod+"] {
            assert!(parse_key_sequence(bad).is_err(), "{bad}");
        }
        assert_eq!(parse_key_sequence("mod+k mod+r").unwrap(), 2);
    }

    #[test]
    fn when_grammar() {
        for ok in [
            "",
            "always",
            "global",
            "not-input",
            "terminal-focus",
            "!inputFocus && !paletteOpen",
            "filesFocus && resource =~ '*.{ts,rs}'",
            "ngwaItemFocus && (ngwaItemKind == 'k1' || ngwaItemKind == \"k2\")",
            "!paneKind == 'terminal'",
            "paletteOpen == true",
            "a && (b || !(c != 'x'))",
        ] {
            assert!(check_when(ok).is_ok(), "{ok}");
        }
        for bad in [
            "global && paneFocus",
            "paneKind == artifact",
            "resource =~ true",
            "(filesFocus",
            "filesFocus &&",
            "always == 'x'",
            "PaneFocus",
            "files-focus",
            "a & b",
            "'unterminated",
        ] {
            assert!(check_when(bad).is_err(), "{bad}");
        }
        assert!(check_when(&"a && ".repeat(200)).is_err());
        assert!(check_when(&format!("{}a{}", "(".repeat(40), ")".repeat(40))).is_err());
        assert_eq!(check_when("not-input").unwrap(), vec!["inputFocus".to_string()]);
    }

    #[test]
    fn unknown_commands_and_context_keys_warn() {
        let known: HashSet<String> = ["explain-file".to_string()].into_iter().collect();
        let c = ValidateContext {
            kind: FileKind::Keybindings,
            scope: SettingsScope::Personal,
            user_action_ids: Some(&known),
        };
        let document = json!({ "version": 1, "bindings": [
            { "key": "mod+shift+e", "command": "explain-file", "when": "fooFocus" },
            { "key": "mod+shift+r", "command": "release-status" },
            { "key": "mod+shift+t", "command": "delete" }
        ]});
        let v = validate_document(c, &document);
        assert!(v.is_ok());
        assert_eq!(codes(&v.warnings), vec!["W_UNKNOWN_CONTEXT_KEY", "W_UNKNOWN_COMMAND"]);
        let document = json!({ "version": 1, "bindings": [
            { "key": "mod+shift+e", "command": "action:explain-file" }
        ]});
        assert_eq!(codes(&validate_document(c, &document).errors), vec!["E_FIELD"]);
    }

    #[test]
    fn template_variables_are_extracted_verbatim() {
        assert_eq!(
            template_variables("a {{file.path}} b {{ selection }} {{branch"),
            vec!["file.path".to_string(), " selection ".to_string()]
        );
    }
}
