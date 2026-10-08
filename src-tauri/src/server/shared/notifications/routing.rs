//! Permission routing core (G-ACCESS §5.3–§5.7, WP-75; DEC-79, DEC-83,
//! DEC-85). It lives here, in the notification core both binaries compile,
//! so the daemon's `permission_decide` arm, the desktop's in-process
//! `permission_decide` command and the `rpc_handler` post-hook share one
//! copy (review M-1):
//!
//! * [`classify`] — the sensitive-ask classifier (§5.3, P-29), stored on the
//!   row as `shell_notifications.sensitive` (0 / 1 / 2) when it is created
//!   ([`record_ask`]);
//! * [`can_decide`] — §5.4's normative algorithm, pure, shared by the decide
//!   core and the read model;
//! * [`decide_with`] — the decide core: dispatches on the row's `dedupe_key`
//!   prefix through registered resolvers ([`AskResolvers`]), writes
//!   `decided_*`, and never auto-allows (§5.6);
//! * [`annotate`] — `can_decide` / `waiting_on` on `notifications_list` rows
//!   (the one `annotate`, called by `access::postfilter`);
//! * the T0 desktop → daemon ask relay (§5.5 (a), N-8 decided (a)+(c),
//!   DEC-83): [`Relay`] behind `permission_relay_{put,take,resolve}`;
//! * the per-socket prompt attribution `chat_ws` captures at the handshake
//!   (§5.7, [`set_prompt_context`]) — what a daemon engine that raises asks
//!   attributes them with. Under T1 no daemon engine raises asks yet
//!   (§5.5 (c)), so the T1 path is exercised by the fixture producer in the
//!   tests below.
//!
//! **Runtimes.** The RPC entry points ([`decide_rpc`], [`relay_rpc`],
//! [`decide_local`]) reach their state through one process-wide runtime:
//! the T0 daemon installs [`DaemonRouting`] at boot ([`install_daemon`]),
//! the desktop installs [`LocalRouting`] when its notification forwarder
//! starts (`crate::notifications`). Every core function takes its pool and
//! resolvers explicitly, so the tests run without either.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::Row;

use super::{ChangeReason, Coalesce, NewNotification, Notification, RecordOutcome};
use crate::access::audit::Event;
use crate::access::{AccessCtx, AccessError, AccessStore, Cap, CapSet, Code, ShareCtx, Tier, Via};

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ─── §5.3 classify ──────────────────────────────────────────────────────────

/// `classify(tool_name, tool_input, project_root)` (§5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Sensitivity {
    pub sensitive: bool,
    /// Rule 3 alone (secret material). Implies `sensitive`.
    pub secret_material: bool,
}

impl Sensitivity {
    pub const NONE: Sensitivity = Sensitivity {
        sensitive: false,
        secret_material: false,
    };
    pub const SENSITIVE: Sensitivity = Sensitivity {
        sensitive: true,
        secret_material: false,
    };
    pub const SECRET: Sensitivity = Sensitivity {
        sensitive: true,
        secret_material: true,
    };

    /// `shell_notifications.sensitive`: 0 = no, 1 = sensitive, 2 = secret
    /// material.
    pub fn level(self) -> i64 {
        if self.secret_material {
            2
        } else if self.sensitive {
            1
        } else {
            0
        }
    }

    pub fn from_level(level: i64) -> Self {
        match level {
            0 => Self::NONE,
            1 => Self::SENSITIVE,
            // Anything else (2, or a value a newer build wrote) fails closed.
            _ => Self::SECRET,
        }
    }
}

/// Rule 1: shell exec tools.
const SHELL_TOOLS: &[&str] = &["Bash", "BashOutput", "KillShell", "KillBash"];
const SHELL_SUFFIXES: &[&str] = &["__exec", "__shell", "__run_command"];
/// Rule 2: the path-like arguments.
const PATH_KEYS: &[&str] = &["file_path", "path", "notebook_path", "cwd"];
/// Rule 3: tool input text naming the vault's env export.
const SECRET_ENV_MARK: &str = "IKENGA_SECRET_";

/// MCP server keys (`mcpServers` keys, e.g. `pkg-<slug>-<name>`) served by
/// a pkg whose manifest declares secrets (§5.3 rule 3, third bullet). The
/// host fills it ([`set_secret_mcp_servers`]); empty means none known.
static SECRET_MCP_SERVERS: RwLock<Option<HashSet<String>>> = RwLock::new(None);

/// Replace the set of MCP server keys whose tools count as secret material.
pub fn set_secret_mcp_servers(keys: impl IntoIterator<Item = String>) {
    let set: HashSet<String> = keys.into_iter().collect();
    if let Ok(mut g) = SECRET_MCP_SERVERS.write() {
        *g = Some(set);
    }
}

fn mcp_server_of(tool_name: &str) -> Option<&str> {
    let rest = tool_name.strip_prefix("mcp__")?;
    rest.split_once("__").map(|(server, _)| server)
}

fn is_secret_mcp(tool_name: &str) -> bool {
    let Some(server) = mcp_server_of(tool_name) else {
        return false;
    };
    SECRET_MCP_SERVERS
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.contains(server)))
        .unwrap_or(false)
}

/// The sensitive-ask classifier (§5.3, P-29). Pure apart from the
/// secret-MCP registry; see [`classify_with`].
pub fn classify(tool_name: &str, tool_input: &Value, project_root: Option<&str>) -> Sensitivity {
    classify_with(tool_name, tool_input, project_root, &is_secret_mcp)
}

/// [`classify`] with an explicit "is this MCP tool served by a pkg that
/// declares secrets" predicate (the table tests pass their own).
pub fn classify_with(
    tool_name: &str,
    tool_input: &Value,
    project_root: Option<&str>,
    secret_mcp: &dyn Fn(&str) -> bool,
) -> Sensitivity {
    let paths = path_args(tool_input);
    let root = project_root
        .filter(|r| !r.trim().is_empty())
        .map(|r| lexical_normalize(Path::new(r)));
    // Where each argument really lands on this host (review WP75-R1): a
    // symlink inside the project can point outside it, or at secret
    // material. `None` when the host can't resolve it (the lexical form
    // alone then decides).
    let real_root = root.as_deref().and_then(canonical_existing);
    let real = |raw: &str| -> Option<PathBuf> {
        let abs = if raw.starts_with('~') || Path::new(raw).is_absolute() {
            absolute_lexical(raw)?
        } else {
            resolve_under(root.as_deref()?, raw)?
        };
        canonical_existing(&abs)
    };
    let resolves_to_secret =
        |raw: &str| real(raw).is_some_and(|r| is_secret_path(&r.to_string_lossy()));
    let tokens = command_tokens(tool_input);

    // Rule 3 — secret material: the argument as written **or** where it
    // resolves.
    let secret = paths
        .iter()
        .any(|p| is_secret_path(p) || resolves_to_secret(p))
        || tokens.iter().any(|t| is_secret_path(t))
        || tokens
            .iter()
            .take(MAX_RESOLVED_TOKENS)
            .any(|t| resolves_to_secret(t))
        || input_text_mentions_secret_env(tool_input)
        || secret_mcp(tool_name);
    if secret {
        return Sensitivity::SECRET;
    }

    // Rule 1 — shell exec.
    let shell =
        SHELL_TOOLS.contains(&tool_name) || SHELL_SUFFIXES.iter().any(|s| tool_name.ends_with(s));

    // Rule 2 — outside the project, or the project root is unknown (fail
    // closed: nothing can be shown to be inside an unknown root). Outside
    // if **either** the lexical or the resolved form is.
    let outside = match &root {
        None => true,
        Some(root) => paths.iter().any(|p| match resolve_under(root, p) {
            None => true,
            Some(abs) if !abs.starts_with(root) => true,
            Some(abs) => match (canonical_existing(&abs), &real_root) {
                (Some(real_abs), Some(real_root)) => !real_abs.starts_with(real_root),
                _ => false,
            },
        }),
    };

    if shell || outside {
        Sensitivity::SENSITIVE
    } else {
        Sensitivity::NONE
    }
}

/// A terminal `PermissionRequest` (Claude Code's own prompt) is shell exec
/// by rule 1, plus whatever its tool input says.
pub fn classify_terminal(
    tool_name: &str,
    tool_input: &Value,
    project_root: Option<&str>,
) -> Sensitivity {
    let s = classify(tool_name, tool_input, project_root);
    Sensitivity {
        sensitive: true,
        ..s
    }
}

fn path_args(input: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(obj) = input.as_object() else {
        return out;
    };
    for key in PATH_KEYS {
        if let Some(s) = obj.get(*key).and_then(Value::as_str) {
            if !s.is_empty() {
                out.push(s.to_string());
            }
        }
    }
    if let Some(list) = obj.get("paths").and_then(Value::as_array) {
        out.extend(
            list.iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        );
    }
    out
}

/// Most Bash tokens resolved on the filesystem per ask (the lexical check
/// covers every token).
const MAX_RESOLVED_TOKENS: usize = 64;

/// Whitespace / shell-metacharacter tokens of a Bash `command`, checked
/// against the secret-path patterns ("a Bash command whose arguments match
/// the §5.3 patterns" is secret material, §4.5.1). Quote characters and
/// backslash escapes are **removed**, not split on, so shell quoting can't
/// break a name apart (`cat .e"nv"` is `cat .env`; review WP75-R8).
fn command_tokens(input: &Value) -> Vec<String> {
    let unquoted: String = input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .filter(|c| !matches!(c, '"' | '\'' | '\\'))
        .collect();
    unquoted
        .split(|c: char| {
            c.is_whitespace()
                || matches!(c, ';' | '|' | '&' | '<' | '>' | '(' | ')' | '`' | '=' | ',')
        })
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

fn input_text_mentions_secret_env(input: &Value) -> bool {
    match input {
        Value::Null => false,
        Value::String(s) => s.contains(SECRET_ENV_MARK),
        other => other.to_string().contains(SECRET_ENV_MARK),
    }
}

/// `**/.env*`, `**/*.pem`, `**/*.key`, `**/id_*`, `**/.ssh/**`,
/// `**/.aws/**`, `**/.gnupg/**`, `**/.netrc`, `**/.ikenga/secrets*`.
pub fn is_secret_path(raw: &str) -> bool {
    let comps: Vec<&str> = raw
        .split(['/', '\\'])
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    let Some(base) = comps.last().copied() else {
        return false;
    };
    if base.starts_with(".env")
        || base.ends_with(".pem")
        || base.ends_with(".key")
        || base.starts_with("id_")
        || base == ".netrc"
    {
        return true;
    }
    if comps
        .iter()
        .any(|c| matches!(*c, ".ssh" | ".aws" | ".gnupg"))
    {
        return true;
    }
    comps
        .windows(2)
        .any(|w| w[0] == ".ikenga" && w[1].starts_with("secrets"))
}

/// `.` / `..` collapsed without touching the filesystem (the host may not
/// even have the path: a remote ask is classified where it is recorded). A
/// `..` that climbs above the root is kept, so the result is not under it.
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(c),
            },
            other => out.push(other),
        }
    }
    out.iter().collect()
}

/// An absolute (or `~/`-relative) argument, lexically normalised; `None`
/// for a relative one when there is no root to resolve it against.
fn absolute_lexical(raw: &str) -> Option<PathBuf> {
    if let Some(rest) = raw.strip_prefix("~/") {
        let home = std::env::var_os("HOME")?;
        return Some(lexical_normalize(&Path::new(&home).join(rest)));
    }
    let p = Path::new(raw);
    p.is_absolute().then(|| lexical_normalize(p))
}

/// Where `abs` really is on this host: the deepest existing ancestor
/// canonicalised (symlinks followed), with the rest appended and
/// normalised. `None` when not even the filesystem root resolves (or `abs`
/// is relative). Paths the host doesn't have come back as written below
/// their deepest real ancestor, so a remote-only path classifies lexically.
fn canonical_existing(abs: &Path) -> Option<PathBuf> {
    if !abs.is_absolute() {
        return None;
    }
    let mut base = abs;
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(base) {
            let mut out = real;
            for c in rest.iter().rev() {
                out.push(c);
            }
            return Some(lexical_normalize(&out));
        }
        let name = base.file_name()?;
        rest.push(name);
        base = base.parent()?;
    }
}

/// `raw` resolved against `root` (relative paths join it). `None` for a
/// home-relative `~` path, which is never inside a project root.
fn resolve_under(root: &Path, raw: &str) -> Option<PathBuf> {
    if raw.starts_with('~') {
        return None;
    }
    let p = Path::new(raw);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    Some(lexical_normalize(&joined))
}

// ─── attribution (§5.7) ─────────────────────────────────────────────────────

/// What a producer stores on a `permission` row beside WP-40's columns
/// (migration 0070): who raised it, which project, how sensitive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attribution {
    /// The member whose dispatch raised the ask; `None` for the Owner's own
    /// work.
    pub requested_by: Option<String>,
    pub project_id: Option<String>,
    pub sensitivity: Sensitivity,
}

/// The read-model copy of the attribution, kept in `action.routing` so the
/// post-hook ([`annotate`]) can work from the listed rows alone (the WP-40
/// list does not select 0070's columns, which stay the store of record).
fn routing_json(a: &Attribution) -> Value {
    json!({
        "sensitive": a.sensitivity.level(),
        "projectId": a.project_id,
        "requestedBy": a.requested_by,
    })
}

/// Record a `permission` row with its attribution (§5.7): [`super::record`]
/// plus the 0070 columns. Returns the stored row, or `None` when the dedupe
/// key suppressed it.
pub async fn record_ask(
    pool: &sqlx::SqlitePool,
    mut new: NewNotification,
    attribution: &Attribution,
) -> Result<Option<Notification>, String> {
    let mut action = new.action.take().unwrap_or_else(|| json!({}));
    if let Value::Object(map) = &mut action {
        map.insert("routing".into(), routing_json(attribution));
    }
    new.action = Some(action);
    let stored = match super::record(pool, new).await? {
        RecordOutcome::Inserted(n) | RecordOutcome::Coalesced(n) | RecordOutcome::Updated(n) => n,
        RecordOutcome::Suppressed => return Ok(None),
    };
    sqlx::query(
        "UPDATE shell_notifications SET requested_by = ?, project_id = ?, sensitive = ? \
         WHERE id = ?",
    )
    .bind(&attribution.requested_by)
    .bind(&attribution.project_id)
    .bind(attribution.sensitivity.level())
    .bind(stored.id)
    .execute(pool)
    .await
    .map_err(|e| format!("notifications attribution: {e}"))?;
    Ok(Some(stored))
}

/// The project whose `root_path` holds `path` (longest root wins): its id
/// and root. Lexical, like [`classify`].
pub async fn project_for_path(pool: &sqlx::SqlitePool, path: &str) -> Option<(String, String)> {
    if path.trim().is_empty() {
        return None;
    }
    let rows = sqlx::query(
        "SELECT id, root_path FROM projects WHERE archived_at IS NULL AND root_path IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    .ok()?;
    let target = lexical_normalize(Path::new(path));
    rows.iter()
        .filter_map(|r| {
            let id: String = r.try_get("id").ok()?;
            let root: String = r.try_get("root_path").ok()?;
            let norm = lexical_normalize(Path::new(&root));
            (!root.trim().is_empty() && target.starts_with(&norm)).then_some((id, root, norm))
        })
        .max_by_key(|(_, _, norm)| norm.components().count())
        .map(|(id, root, _)| (id, root))
}

/// Attribution captured at a `/ws/chat` handshake / prompt (§5.7): the
/// share member and project (`X-Ikenga-Share-*`), or the own-workspace
/// project from the thread's cwd. A daemon engine that raises asks reads it
/// by thread id when it records one ([`record_ask_for_thread`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptContext {
    pub requested_by: Option<String>,
    pub project_id: Option<String>,
    /// The project root asks are classified against (§5.3 rule 2).
    pub project_root: Option<String>,
}

/// One live `/ws/chat` socket's context on a thread: `(socket, seq, ctx)`.
type SocketContexts = Vec<(u64, u64, PromptContext)>;

fn prompt_contexts() -> &'static Mutex<(u64, HashMap<String, SocketContexts>)> {
    static MAP: OnceLock<Mutex<(u64, HashMap<String, SocketContexts>)>> = OnceLock::new();
    MAP.get_or_init(Default::default)
}

/// A fresh id for one `/ws/chat` socket: its prompt contexts are kept and
/// cleared under it, so two sockets on one thread (the Owner and a member
/// through a share) never overwrite or wipe each other's (review WP75-R11).
pub fn prompt_socket() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Remember `socket`'s attribution on `thread_id` for the turn about to run.
pub fn set_prompt_context(thread_id: &str, socket: u64, ctx: PromptContext) {
    if let Ok(mut g) = prompt_contexts().lock() {
        let (seq, map) = &mut *g;
        *seq += 1;
        let live = map.entry(thread_id.to_string()).or_default();
        live.retain(|(s, _, _)| *s != socket);
        live.push((socket, *seq, ctx));
    }
}

/// Forget `socket`'s context on `thread_id` (that socket closed); other
/// sockets' stay.
pub fn clear_prompt_context(thread_id: &str, socket: u64) {
    if let Ok(mut g) = prompt_contexts().lock() {
        let map = &mut g.1;
        if let Some(live) = map.get_mut(thread_id) {
            live.retain(|(s, _, _)| *s != socket);
            if live.is_empty() {
                map.remove(thread_id);
            }
        }
    }
}

/// The attribution an ask raised on `thread_id` carries. With one live
/// context (or several that agree) that one. When live sockets on the
/// thread disagree, the ask can't be tied to one requester, so it fails
/// closed: no member, no project (§5.4 3a: no share may decide it — only
/// the Owner), no root (§5.3 rule 2: sensitive).
pub fn prompt_context(thread_id: &str) -> Option<PromptContext> {
    let g = prompt_contexts().lock().ok()?;
    let live = g.1.get(thread_id)?;
    let (_, _, first) = live.first()?;
    if live.iter().all(|(_, _, c)| c == first) {
        Some(first.clone())
    } else {
        Some(PromptContext::default())
    }
}

/// The producer path a daemon engine uses (and the T1 fixture producer the
/// tests drive, DEC-83): classify against the thread's project root and
/// record with the captured attribution.
pub async fn record_ask_for_thread(
    pool: &sqlx::SqlitePool,
    thread_id: &str,
    new: NewNotification,
    tool_name: &str,
    tool_input: &Value,
) -> Result<Option<Notification>, String> {
    let ctx = prompt_context(thread_id).unwrap_or_default();
    let attribution = Attribution {
        requested_by: ctx.requested_by,
        project_id: ctx.project_id,
        sensitivity: classify(tool_name, tool_input, ctx.project_root.as_deref()),
    };
    record_ask(pool, new, &attribution).await
}

// ─── the ask a row points at ────────────────────────────────────────────────

/// What a `permission` row's `dedupe_key` names (§5.5 table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskKey {
    /// `permission:hook:<request_id>` — the held hook gate (desktop).
    Hook {
        request_id: String,
    },
    /// `permission:acp:<thread>:<request>` — a Claude Code ACP round-trip
    /// (desktop).
    Acp {
        thread_id: String,
        request_id: String,
    },
    /// `permission:relay:<desktop key>` — the daemon's mirror of a desktop
    /// ask (§5.5 (a)).
    Relay {
        key: String,
    },
    /// `permission:terminal:*` — Claude Code's own prompt: open-only.
    Terminal,
    Other,
}

impl AskKey {
    pub fn parse(dedupe_key: &str) -> AskKey {
        if let Some(id) = dedupe_key.strip_prefix("permission:hook:") {
            if !id.is_empty() {
                return AskKey::Hook {
                    request_id: id.to_string(),
                };
            }
        } else if let Some(rest) = dedupe_key.strip_prefix("permission:acp:") {
            // Thread ids are UUIDs (no ':'); the request id is the rest.
            if let Some((thread, request)) = rest.split_once(':') {
                if !thread.is_empty() && !request.is_empty() {
                    return AskKey::Acp {
                        thread_id: thread.to_string(),
                        request_id: request.to_string(),
                    };
                }
            }
        } else if let Some(key) = dedupe_key.strip_prefix("permission:relay:") {
            if !key.is_empty() {
                return AskKey::Relay {
                    key: key.to_string(),
                };
            }
        } else if dedupe_key.starts_with("permission:terminal:") {
            return AskKey::Terminal;
        }
        AskKey::Other
    }

    /// Whether the engine behind this ask offers "always for this project"
    /// (§5.5 decision mapping): the ACP round-trip does (`allow_always`), the
    /// hook gate does not; a relay mirror offers what its desktop ask does.
    pub fn offers_always(&self) -> bool {
        match self {
            AskKey::Acp { .. } => true,
            AskKey::Relay { key } => AskKey::parse(key).offers_always(),
            _ => false,
        }
    }
}

/// `permission_decide`'s `decision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    AllowOnce,
    AllowAlwaysProject,
    Deny,
}

impl Decision {
    pub fn parse(s: &str) -> Option<Decision> {
        match s {
            "allow_once" => Some(Decision::AllowOnce),
            "allow_always_project" => Some(Decision::AllowAlwaysProject),
            "deny" => Some(Decision::Deny),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Decision::AllowOnce => "allow_once",
            Decision::AllowAlwaysProject => "allow_always_project",
            Decision::Deny => "deny",
        }
    }

    pub fn allows(self) -> bool {
        !matches!(self, Decision::Deny)
    }
}

/// Who decided (the `decided_*` columns, §5.7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecidedBy {
    pub principal_id: Option<String>,
    /// `session` | `device` | `operator`.
    pub via: &'static str,
    pub device_id: Option<String>,
}

/// The resolver table `decide` dispatches through (§5.5): the desktop
/// registers the hook and ACP resolvers, the daemon only the relay one. A
/// prefix with no registered resolver gets `not_found`.
pub trait AskResolvers: Send + Sync {
    /// Answer the live ask behind `key`. `None`: no resolver for this
    /// prefix here. `Some(Err(conflict))`: the ask is already over (timed
    /// out under the decider) — the row stays resolved.
    fn resolve<'a>(
        &'a self,
        key: &'a AskKey,
        decision: Decision,
        by: &'a DecidedBy,
    ) -> Option<BoxFuture<'a, Result<(), AccessError>>>;
}

/// No resolvers (a T1 child today: no daemon engine raises asks, §5.5 (c)).
pub struct NoResolvers;

impl AskResolvers for NoResolvers {
    fn resolve<'a>(
        &'a self,
        _key: &'a AskKey,
        _decision: Decision,
        _by: &'a DecidedBy,
    ) -> Option<BoxFuture<'a, Result<(), AccessError>>> {
        None
    }
}

// ─── §5.4 who may decide ────────────────────────────────────────────────────

/// The deciding credential, reduced to what §5.4 reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decider {
    pub by: DecidedBy,
    /// Effective caps (§1.4: routing already applied).
    pub caps: CapSet,
    pub tier: Tier,
    pub share: Option<ShareCtx>,
    /// The routing preference (§5.1) withheld `approve` from `caps`, as
    /// recorded where the caps were computed (review WP75-R5).
    pub routing_withheld: bool,
}

impl Decider {
    pub fn from_ctx(ctx: &AccessCtx) -> Decider {
        let share = ctx.share.clone();
        let (principal_id, device_id) = match &share {
            Some(s) => (
                s.member_principal_id.clone(),
                s.member_device_id.clone().or_else(|| ctx.device_id.clone()),
            ),
            None => (Some(ctx.principal_id.to_string()), ctx.device_id.clone()),
        };
        let via = match &ctx.via {
            Via::Session { .. } => "session",
            Via::Device { .. } => "device",
            Via::Operator => "operator",
            // A relayed principal request on a T1 child: a device when the
            // broker named one, else a password session.
            Via::ChildToken | Via::Relayed => {
                if device_id.is_some() {
                    "device"
                } else {
                    "session"
                }
            }
        };
        Decider {
            by: DecidedBy {
                principal_id,
                via,
                device_id,
            },
            caps: ctx.caps,
            tier: ctx.tier,
            share,
            routing_withheld: ctx.meta.routing_withheld_approve,
        }
    }

    /// The desktop deciding its own asks in-process (§5.5): the operator on
    /// the host device, `full`, `approve` only if the principal's routing
    /// preference admits the host (§5.1).
    pub fn host(routing_ok: bool, owner: Option<String>, host_device: Option<String>) -> Decider {
        let caps = if routing_ok {
            CapSet::ALL
        } else {
            CapSet::ALL.without(Cap::Approve)
        };
        Decider {
            by: DecidedBy {
                principal_id: owner,
                via: "operator",
                device_id: host_device,
            },
            caps,
            tier: Tier::Full,
            share: None,
            routing_withheld: !routing_ok,
        }
    }
}

/// The fields of a `permission` row §5.4 reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskRow {
    pub id: i64,
    pub dedupe_key: Option<String>,
    pub title: String,
    pub resolved_at: Option<i64>,
    pub project_id: Option<String>,
    pub requested_by: Option<String>,
    pub sensitivity: Sensitivity,
}

/// Why a decider may not decide a row (§5.4), in the algorithm's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Step 1, routing: `approve` removed by the routing preference.
    RoutingRefused,
    /// Step 1: the tier / role holds no `approve`.
    MissingApprove,
    /// Step 2.
    AlreadyResolved,
    /// Step 3a.
    NotInShare,
    /// Steps 3b / 3c.
    OwnerApprovalRequired { secret_material: bool },
    /// Step 3d.
    MembersCannotPersist,
}

impl Refusal {
    pub fn to_error(self) -> AccessError {
        match self {
            Refusal::RoutingRefused => AccessError::new(
                Code::RoutingRefused,
                "permission asks are answered on another device (this device only)",
            ),
            Refusal::MissingApprove => AccessError::missing(CapSet::of(&[Cap::Approve])),
            Refusal::AlreadyResolved => {
                AccessError::new(Code::Conflict, "this ask was already answered or timed out")
            }
            Refusal::NotInShare => AccessError::new(Code::NotFound, "no such ask"),
            Refusal::OwnerApprovalRequired { secret_material } => AccessError::new(
                Code::OwnerApprovalRequired,
                if secret_material {
                    "asks that touch secrets go to the Owner"
                } else {
                    "this project requires the Owner to answer sensitive asks"
                },
            ),
            Refusal::MembersCannotPersist => AccessError::new(
                Code::Forbidden,
                "class=owner — members can't add an always-allow rule",
            ),
        }
    }

    /// The `permission.refused {reason}` this refusal is audited with, if
    /// any (§6.5).
    pub fn audit_reason(self) -> Option<&'static str> {
        match self {
            Refusal::RoutingRefused => Some("routing_refused"),
            Refusal::OwnerApprovalRequired { .. } => Some("owner_approval_required"),
            Refusal::MissingApprove | Refusal::MembersCannotPersist => Some("forbidden"),
            Refusal::AlreadyResolved | Refusal::NotInShare => None,
        }
    }
}

/// §5.4, normative. `decision` is `None` for the read model (step 3d only
/// applies to a concrete `allow_always_project`).
pub fn can_decide(d: &Decider, row: &AskRow, decision: Option<Decision>) -> Result<(), Refusal> {
    // 1. shared{approve}; effective caps already include routing_ok.
    if !d.caps.contains(Cap::Approve) {
        return Err(if d.routing_withheld {
            Refusal::RoutingRefused
        } else {
            Refusal::MissingApprove
        });
    }
    // 2.
    if row.resolved_at.is_some() {
        return Err(Refusal::AlreadyResolved);
    }
    // 3.
    if let Some(share) = &d.share {
        // a.
        if row.project_id.as_deref() != Some(share.project_id.as_str()) {
            return Err(Refusal::NotInShare);
        }
        // c. (checked before b so the reason names secrets) — whatever the
        // policy (N-10 default).
        if row.sensitivity.secret_material {
            return Err(Refusal::OwnerApprovalRequired {
                secret_material: true,
            });
        }
        // b.
        if row.sensitivity.sensitive && share.owner_approval {
            return Err(Refusal::OwnerApprovalRequired {
                secret_material: false,
            });
        }
        // d.
        if decision == Some(Decision::AllowAlwaysProject) {
            return Err(Refusal::MembersCannotPersist);
        }
    }
    // 4. own workspace: the Owner.
    Ok(())
}

/// `waiting_on` (§5.7): what is missing for this decider, or `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitingOn {
    Owner,
    Device,
    Approve,
}

pub fn waiting_on(refusal: Refusal) -> Option<WaitingOn> {
    match refusal {
        Refusal::RoutingRefused => Some(WaitingOn::Device),
        Refusal::MissingApprove => Some(WaitingOn::Approve),
        Refusal::OwnerApprovalRequired { .. } => Some(WaitingOn::Owner),
        Refusal::AlreadyResolved | Refusal::NotInShare | Refusal::MembersCannotPersist => None,
    }
}

// ─── the decide core ────────────────────────────────────────────────────────

/// One `permission_decide`, for the caller to answer and audit.
#[derive(Debug)]
pub struct DecideReport {
    pub result: Result<(), AccessError>,
    pub decision: Option<Decision>,
    pub row: Option<AskRow>,
    /// `permission.refused {reason}` to audit, when refused for a reason
    /// §6.5 records.
    pub refused: Option<&'static str>,
}

impl DecideReport {
    fn err(e: AccessError) -> Self {
        Self {
            result: Err(e),
            decision: None,
            row: None,
            refused: None,
        }
    }

    pub fn into_value(self) -> Result<Value, AccessError> {
        self.result.map(|()| json!({ "resolved": true }))
    }
}

pub async fn load_ask(pool: &sqlx::SqlitePool, id: i64) -> Result<Option<AskRow>, String> {
    let row = sqlx::query(
        "SELECT id, kind, dedupe_key, title, resolved_at, project_id, requested_by, sensitive \
         FROM shell_notifications WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("notifications ask lookup: {e}"))?;
    let Some(r) = row else { return Ok(None) };
    let kind: String = r.try_get("kind").map_err(|e| e.to_string())?;
    if kind != "permission" {
        return Ok(None);
    }
    Ok(Some(AskRow {
        id: r.try_get("id").map_err(|e| e.to_string())?,
        dedupe_key: r.try_get("dedupe_key").map_err(|e| e.to_string())?,
        title: r.try_get("title").map_err(|e| e.to_string())?,
        resolved_at: r.try_get("resolved_at").map_err(|e| e.to_string())?,
        project_id: r.try_get("project_id").map_err(|e| e.to_string())?,
        requested_by: r.try_get("requested_by").map_err(|e| e.to_string())?,
        sensitivity: Sensitivity::from_level(
            r.try_get::<i64, _>("sensitive")
                .map_err(|e| e.to_string())?,
        ),
    }))
}

/// The open `permission` row carrying `key`, if any.
pub async fn find_open_by_key(pool: &sqlx::SqlitePool, key: &str) -> Option<i64> {
    sqlx::query_scalar(
        "SELECT id FROM shell_notifications \
         WHERE dedupe_key = ? AND kind = 'permission' AND resolved_at IS NULL \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(key)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}

/// Claim an open row for `by` (resolved + `decided_*`), atomically: the
/// second of two racing deciders gets `false`.
async fn claim(pool: &sqlx::SqlitePool, id: i64, by: &DecidedBy) -> Result<bool, AccessError> {
    let now = now_ms();
    let n = sqlx::query(
        "UPDATE shell_notifications \
         SET resolved_at = ?, read_at = COALESCE(read_at, ?), \
             decided_by = ?, decided_via = ?, decided_device = ? \
         WHERE id = ? AND resolved_at IS NULL",
    )
    .bind(now)
    .bind(now)
    .bind(&by.principal_id)
    .bind(by.via)
    .bind(&by.device_id)
    .bind(id)
    .execute(pool)
    .await
    .map_err(AccessError::internal)?
    .rows_affected();
    Ok(n == 1)
}

/// Undo a claim whose resolver failed. `ask_over`: the ask itself is gone
/// (timed out under us) — the row stays resolved, only the attribution is
/// dropped (§5.6: never a decision nobody made).
async fn unclaim(pool: &sqlx::SqlitePool, id: i64, ask_over: bool) {
    let sql = if ask_over {
        "UPDATE shell_notifications \
         SET decided_by = NULL, decided_via = NULL, decided_device = NULL WHERE id = ?"
    } else {
        "UPDATE shell_notifications \
         SET resolved_at = NULL, decided_by = NULL, decided_via = NULL, decided_device = NULL \
         WHERE id = ?"
    };
    if let Err(e) = sqlx::query(sql).bind(id).execute(pool).await {
        log::warn!(target: "ikenga::notifications", "permission unclaim {id}: {e}");
    }
}

/// `permission_decide`'s core (§5.4, §5.5). Never extends a timeout and
/// never falls back to another answerer (§5.6): a refusal leaves the ask
/// where it was.
pub async fn decide_with(
    pool: &sqlx::SqlitePool,
    d: &Decider,
    id: i64,
    decision: &str,
    resolvers: &dyn AskResolvers,
) -> DecideReport {
    let Some(decision) = Decision::parse(decision) else {
        return DecideReport::err(AccessError::new(
            Code::InvalidRequest,
            "decision must be allow_once | allow_always_project | deny",
        ));
    };
    let row = match load_ask(pool, id).await {
        Ok(Some(r)) => r,
        Ok(None) => return DecideReport::err(AccessError::new(Code::NotFound, "no such ask")),
        Err(e) => return DecideReport::err(AccessError::internal(e)),
    };
    let mut report = DecideReport {
        result: Ok(()),
        decision: Some(decision),
        row: Some(row.clone()),
        refused: None,
    };
    if let Err(refusal) = can_decide(d, &row, Some(decision)) {
        report.refused = refusal.audit_reason();
        report.result = Err(refusal.to_error());
        return report;
    }
    let key = AskKey::parse(row.dedupe_key.as_deref().unwrap_or(""));
    match key {
        AskKey::Terminal => {
            report.result = Err(AccessError::new(
                Code::AnswerInTerminal,
                "Claude Code is asking in its terminal; answer it there",
            ));
            return report;
        }
        AskKey::Other => {
            report.result = Err(AccessError::new(Code::NotFound, "no such ask"));
            return report;
        }
        _ => {}
    }
    if decision == Decision::AllowAlwaysProject && !key.offers_always() {
        report.result = Err(AccessError::new(
            Code::InvalidRequest,
            "this ask has no \"always for this project\" option",
        ));
        return report;
    }
    match claim(pool, row.id, &d.by).await {
        Ok(true) => {}
        Ok(false) => {
            report.result = Err(Refusal::AlreadyResolved.to_error());
            return report;
        }
        Err(e) => {
            report.result = Err(e);
            return report;
        }
    }
    let Some(fut) = resolvers.resolve(&key, decision, &d.by) else {
        unclaim(pool, row.id, false).await;
        report.result = Err(AccessError::new(
            Code::NotFound,
            "this ask can't be answered here",
        ));
        return report;
    };
    if let Err(e) = fut.await {
        unclaim(pool, row.id, e.code == Code::Conflict).await;
        super::publish(ChangeReason::Read, None);
        report.result = Err(e);
        return report;
    }
    super::publish(ChangeReason::Read, None);
    report
}

/// The desktop applying one remote decision taken from the relay
/// (`permission_relay_take`, §5.5 (a)) to its own ask: the daemon already
/// checked §5.4 and audited it (A-39: audited once, by the daemon), so this
/// only claims the local row for the deciding device and resolves the ask.
/// `Ok(false)`: the ask is no longer open here (answered on the host, or
/// timed out) — the caller reports that back (`permission_relay_resolve`)
/// so the daemon retracts the decision.
pub async fn apply_relayed(
    pool: &sqlx::SqlitePool,
    resolvers: &dyn AskResolvers,
    owner: Option<String>,
    decision: &Value,
) -> Result<bool, AccessError> {
    let key = req_str(decision, "key")?;
    let parsed = Decision::parse(req_str(decision, "decision")?)
        .ok_or_else(|| AccessError::new(Code::InvalidRequest, "unknown decision"))?;
    let ask = AskKey::parse(key);
    if !matches!(ask, AskKey::Hook { .. } | AskKey::Acp { .. }) {
        return Err(AccessError::new(
            Code::InvalidRequest,
            "not a desktop ask key",
        ));
    }
    let Some(id) = find_open_by_key(pool, key).await else {
        return Ok(false);
    };
    let by = DecidedBy {
        // The daemon names the deciding principal (review WP75-R4); an
        // older daemon's decision falls back to the host's owner.
        principal_id: opt_str(decision, "decidedBy").or(owner),
        via: match decision.get("decidedVia").and_then(Value::as_str) {
            Some("session") => "session",
            Some("operator") => "operator",
            _ => "device",
        },
        device_id: opt_str(decision, "decidedDevice"),
    };
    if !claim(pool, id, &by).await? {
        return Ok(false);
    }
    let Some(fut) = resolvers.resolve(&ask, parsed, &by) else {
        unclaim(pool, id, false).await;
        return Err(AccessError::new(Code::NotFound, "no resolver for this ask"));
    };
    match fut.await {
        Ok(()) => {
            super::publish(ChangeReason::Read, None);
            Ok(true)
        }
        Err(e) if e.code == Code::Conflict => {
            unclaim(pool, id, true).await;
            super::publish(ChangeReason::Read, None);
            Ok(false)
        }
        Err(e) => {
            unclaim(pool, id, false).await;
            Err(e)
        }
    }
}

/// The `permission.decided` / `permission.refused` audit event for a report
/// (§6.5), or `None` when nothing is recorded.
pub fn audit_event_for(ctx: Option<&AccessCtx>, report: &DecideReport) -> Option<Event> {
    let row = report.row.as_ref();
    let base = |kind: &'static str| match ctx {
        Some(c) => Event::by(kind, c),
        None => Event::new(kind, crate::access::audit::AuditVia::Operator),
    };
    let ev = match (&report.result, report.refused) {
        (Ok(()), _) => base("permission.decided").detail(json!({
            "decision": report.decision.map(Decision::as_str),
            "sensitive": row.map(|r| r.sensitivity.level()).unwrap_or(0),
            "requested_by": row.and_then(|r| r.requested_by.clone()),
        })),
        (Err(_), Some(reason)) => base("permission.refused").detail(json!({ "reason": reason })),
        (Err(_), None) => return None,
    };
    let mut ev = match row {
        Some(r) => ev.target(r.title.chars().take(120).collect::<String>()),
        None => ev,
    };
    if let (Some(c), Some(r)) = (ctx, row) {
        ev.project_key = match (&c.share, &r.project_id) {
            (Some(s), _) => Some(s.project_key.clone()),
            (None, Some(p)) => Some(format!("{}/{p}", c.principal_id)),
            (None, None) => None,
        };
    }
    Some(ev)
}

/// Append one non-access-changing audit row (`permission.*` continue when
/// the chain is degraded, §6.4) in its own transaction. Best effort: the
/// decision already happened.
pub async fn append_audit(store: &AccessStore, ev: &Event) {
    let r = async {
        let mut conn = store.pool().acquire().await?;
        let mut tx = sqlx::Connection::begin_with(&mut *conn, "BEGIN IMMEDIATE").await?;
        let head = store
            .chain()
            .append(&mut tx, ev)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        tx.commit().await?;
        store.chain().committed(head);
        anyhow::Ok(())
    }
    .await;
    if let Err(e) = r {
        tracing::warn!("access audit {}: {e:#}", ev.kind);
    }
}

// ─── the T0 ask relay (§5.5 (a)) ────────────────────────────────────────────

/// Most a relay ask may stay open: the ACP bound (300 s) plus slack.
const RELAY_MAX_TTL_MS: i64 = 10 * 60 * 1000;
/// `permission_relay_take {waitMs}` cap (§9.1).
pub const RELAY_MAX_WAIT_MS: u64 = 25_000;

/// A remote decision waiting for the desktop (`permission_relay_take`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayDecision {
    pub key: String,
    pub decision: &'static str,
    pub decided_via: &'static str,
    pub decided_device: Option<String>,
    /// The deciding principal, server-derived (`Decider.by`), so the
    /// desktop attributes the decision without a cached identity (review
    /// WP75-R4). Additive to §9.1's shape.
    pub decided_by: Option<String>,
}

/// How long after an ask's expiry the daemon still accepts the desktop's
/// "that decision did not take effect" report for it. Long enough to cover
/// a desktop that crashed between `permission_relay_take` and applying the
/// decision and is restarted: its boot sweep ([`sweep_orphaned_asks`])
/// reports the ask then (review WP75-RV2). A delivered decision holds no
/// keep-alive, so this keeps nothing awake.
const RELAY_ACK_GRACE: Duration = Duration::from_secs(5 * 60);

struct PendingAsk {
    row_id: i64,
    _hold: crate::access::KeepAlive,
}

/// A remote decision between `queue` and the end of its ask: queued (in
/// the outbox, holding the daemon awake) or delivered to the desktop.
struct Decided {
    decision: RelayDecision,
    row_id: i64,
    /// `Some` while queued; dropped when taken or expired.
    hold: Option<crate::access::KeepAlive>,
    delivered: bool,
    /// The `permission.decided` row once the daemon wrote it — what a
    /// retraction answers.
    audited: Option<Event>,
}

#[derive(Default)]
struct RelayState {
    pending: HashMap<String, PendingAsk>,
    /// Remote decisions by desktop key, in queue order (`seq`).
    decided: HashMap<String, (u64, Decided)>,
    seq: u64,
}

/// The daemon side of the relay: open mirror asks and remote decisions.
#[derive(Default)]
pub struct Relay {
    state: Mutex<RelayState>,
    notify: tokio::sync::Notify,
    /// The daemon's chain (`None` in tests that don't audit): where a
    /// retraction is written.
    store: Option<AccessStore>,
    /// Orders a decision's audit row before its retraction.
    audit_order: tokio::sync::Mutex<()>,
}

impl Relay {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn with_store(store: AccessStore) -> Arc<Self> {
        Arc::new(Self {
            store: Some(store),
            ..Self::default()
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RelayState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Open relay asks (what keeps the daemon alive, §2.5).
    pub fn pending_count(&self) -> usize {
        self.lock().pending.len()
    }

    /// Whether `key`'s mirror is still answerable in this daemon run. A
    /// mirror row left open by an earlier run is not (review WP75-R7).
    pub fn is_open(&self, key: &str) -> bool {
        self.lock().pending.contains_key(key)
    }

    /// Keep-alives this relay holds: open asks plus decisions not yet taken.
    pub fn held(&self) -> usize {
        let st = self.lock();
        st.pending.len()
            + st.decided
                .values()
                .filter(|(_, d)| d.hold.is_some())
                .count()
    }

    fn mirror_key(key: &str) -> String {
        format!("permission:relay:{key}")
    }

    /// `permission_relay_put` (§5.5 (a)): upsert the mirror row.
    pub async fn put(
        self: &Arc<Self>,
        pool: &sqlx::SqlitePool,
        args: &Value,
    ) -> Result<Value, AccessError> {
        let key = req_str(args, "key")?;
        match AskKey::parse(key) {
            AskKey::Hook { .. } | AskKey::Acp { .. } => {}
            AskKey::Terminal => {
                return Err(AccessError::new(
                    Code::InvalidRequest,
                    "terminal asks are answered in their terminal; they are not relayed",
                ))
            }
            _ => {
                return Err(AccessError::new(
                    Code::InvalidRequest,
                    "key must be a desktop permission key (permission:hook:… | permission:acp:…)",
                ))
            }
        }
        let title = req_str(args, "title")?;
        let body = args.get("body").and_then(Value::as_str).map(str::to_string);
        let now = now_ms();
        let expires = args
            .get("expiresAtMs")
            .and_then(Value::as_i64)
            .ok_or_else(|| AccessError::new(Code::InvalidRequest, "`expiresAtMs` is required"))?;
        if expires <= now {
            return Err(AccessError::new(
                Code::InvalidRequest,
                "the ask has already expired",
            ));
        }
        let expires = expires.min(now + RELAY_MAX_TTL_MS);
        let sensitivity = match args.get("sensitive") {
            Some(Value::Bool(b)) => {
                if *b {
                    Sensitivity::SENSITIVE
                } else {
                    Sensitivity::NONE
                }
            }
            Some(v) => Sensitivity::from_level(v.as_i64().unwrap_or(2)),
            // Fail closed: an unclassified ask is secret material.
            None => Sensitivity::SECRET,
        };
        let attribution = Attribution {
            requested_by: opt_str(args, "requestedBy"),
            project_id: opt_str(args, "projectId"),
            sensitivity,
        };
        let mirror = Self::mirror_key(key);
        let new = NewNotification {
            kind: super::NotificationKind::Permission,
            title: title.chars().take(120).collect(),
            body: body.map(|b| b.chars().take(240).collect()),
            action: Some(json!({
                "kind": "permission.decide",
                "via": "relay",
                "key": key,
                "allowAlways": AskKey::parse(key).offers_always(),
                "expiresAtMs": expires,
            })),
            source: "relay".into(),
            dedupe_key: Some(mirror.clone()),
            coalesce: Coalesce::Once,
        };
        let id = match record_ask(pool, new, &attribution)
            .await
            .map_err(AccessError::internal)?
        {
            Some(n) => n.id,
            // Once: a repeat put of the same ask is idempotent.
            None => sqlx::query_scalar::<_, i64>(
                "SELECT id FROM shell_notifications WHERE dedupe_key = ? ORDER BY id DESC LIMIT 1",
            )
            .bind(&mirror)
            .fetch_one(pool)
            .await
            .map_err(AccessError::internal)?,
        };
        let open = load_ask(pool, id)
            .await
            .map_err(AccessError::internal)?
            .is_some_and(|r| r.resolved_at.is_none());
        if open {
            let fresh = {
                let mut st = self.lock();
                let fresh = !st.pending.contains_key(key) && !st.decided.contains_key(key);
                if fresh {
                    st.pending.insert(
                        key.to_string(),
                        PendingAsk {
                            row_id: id,
                            _hold: crate::access::KeepAlive::hold(),
                        },
                    );
                }
                fresh
            };
            if fresh {
                // §5.6: an unanswered ask times out as denied — the desktop's
                // own bound fires there; here the mirror closes, and a
                // decision the desktop never took is retracted (review
                // WP75-R7: an outbox entry lives no longer than its ask).
                let relay = self.clone();
                let pool = pool.clone();
                let key = key.to_string();
                let wait = Duration::from_millis((expires - now).max(0) as u64);
                tokio::spawn(async move {
                    tokio::time::sleep(wait).await;
                    relay.expire(&pool, &key).await;
                    tokio::time::sleep(RELAY_ACK_GRACE).await;
                    relay.forget(&key);
                });
            }
        }
        Ok(json!({ "id": id }))
    }

    /// Close a mirror ask that is over without a remote decision.
    async fn close(&self, pool: &sqlx::SqlitePool, key: &str) {
        let removed = self.lock().pending.remove(key);
        if removed.is_some() {
            if let Err(e) = super::resolve_by_key(pool, &Self::mirror_key(key)).await {
                log::warn!(target: "ikenga::notifications", "relay close {key}: {e}");
            }
        }
    }

    /// The ask's bound ran out: close an unanswered mirror; retract a
    /// decision still queued (the desktop never took it, so it never took
    /// effect).
    async fn expire(&self, pool: &sqlx::SqlitePool, key: &str) {
        self.close(pool, key).await;
        let queued = self
            .lock()
            .decided
            .get(key)
            .is_some_and(|(_, d)| !d.delivered);
        if queued {
            self.retract(pool, key, "expired").await;
        }
    }

    /// Drop a delivered decision's record once no report can come for it.
    fn forget(&self, key: &str) {
        let mut st = self.lock();
        if st.decided.get(key).is_some_and(|(_, d)| d.delivered) {
            st.decided.remove(key);
        }
    }

    /// A remote decision did not take effect (review WP75-R2): drop it,
    /// clear the mirror's `decided_*` (the row stays resolved — the ask is
    /// over) and, if `permission.decided` was already written for it,
    /// follow it with `permission.refused {reason: not_applied}` so the
    /// chain never claims a decision that didn't happen.
    async fn retract(&self, pool: &sqlx::SqlitePool, key: &str, outcome: &str) {
        let _order = self.audit_order.lock().await;
        let Some((_, d)) = self.lock().decided.remove(key) else {
            return;
        };
        unclaim(pool, d.row_id, true).await;
        super::publish(ChangeReason::Read, None);
        if let (Some(store), Some(ev)) = (&self.store, d.audited) {
            let fix = Event {
                kind: "permission.refused",
                detail: json!({}),
                ..ev
            }
            .detail(json!({
                "reason": "not_applied",
                "outcome": outcome,
                "decision": d.decision.decision,
            }));
            append_audit(store, &fix).await;
        }
    }

    /// Write `permission.decided` for a queued remote decision (A-39:
    /// audited once, by the daemon) — unless it was already retracted, in
    /// which case nothing is claimed.
    pub async fn audit_decided(&self, key: &str, ev: Event) {
        let _order = self.audit_order.lock().await;
        let live = match self.lock().decided.get_mut(key) {
            Some((_, d)) => {
                d.audited = Some(ev.clone());
                true
            }
            None => false,
        };
        if let (true, Some(store)) = (live, &self.store) {
            append_audit(store, &ev).await;
        }
    }

    /// `permission_relay_take {waitMs ≤ 25000}`: every queued decision, or
    /// an empty list after the wait.
    pub async fn take(&self, wait_ms: u64) -> Value {
        let wait = Duration::from_millis(wait_ms.min(RELAY_MAX_WAIT_MS));
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let drained: Vec<RelayDecision> = {
                let mut st = self.lock();
                let mut out: Vec<(u64, RelayDecision)> = st
                    .decided
                    .values_mut()
                    .filter(|(_, d)| !d.delivered)
                    .map(|(seq, d)| {
                        d.delivered = true;
                        d.hold = None;
                        (*seq, d.decision.clone())
                    })
                    .collect();
                out.sort_by_key(|(seq, _)| *seq);
                out.into_iter().map(|(_, d)| d).collect()
            };
            if !drained.is_empty() {
                return json!({ "decisions": drained });
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return json!({ "decisions": [] });
            }
        }
    }

    /// `permission_relay_resolve {key, outcome}`: the desktop resolved or
    /// timed out the ask itself — close the mirror. A remote decision for
    /// the same ask, queued or delivered, did not take effect (the desktop
    /// reports one it applied by saying nothing), so it is retracted.
    pub async fn resolve(
        &self,
        pool: &sqlx::SqlitePool,
        args: &Value,
    ) -> Result<Value, AccessError> {
        let key = req_str(args, "key")?;
        let outcome = req_str(args, "outcome")?;
        if !matches!(outcome, "decided_on_host" | "timed_out" | "cancelled") {
            return Err(AccessError::new(
                Code::InvalidRequest,
                "outcome must be decided_on_host | timed_out | cancelled",
            ));
        }
        self.retract(pool, key, outcome).await;
        self.close(pool, key).await;
        // Already decided remotely: the row is resolved; nothing to do.
        let _ = super::resolve_by_key(pool, &Self::mirror_key(key)).await;
        Ok(json!({}))
    }

    /// The relay resolver: queue a remote decision for the desktop.
    fn queue(&self, key: &str, decision: Decision, by: &DecidedBy) -> Result<(), AccessError> {
        let mut st = self.lock();
        let Some(pending) = st.pending.remove(key) else {
            return Err(AccessError::new(
                Code::Conflict,
                "the ask timed out before the decision reached the host",
            ));
        };
        st.seq += 1;
        let seq = st.seq;
        st.decided.insert(
            key.to_string(),
            (
                seq,
                Decided {
                    decision: RelayDecision {
                        key: key.to_string(),
                        decision: decision.as_str(),
                        decided_via: by.via,
                        decided_device: by.device_id.clone(),
                        decided_by: by.principal_id.clone(),
                    },
                    row_id: pending.row_id,
                    hold: Some(pending._hold),
                    delivered: false,
                    audited: None,
                },
            ),
        );
        drop(st);
        self.notify.notify_one();
        Ok(())
    }

    /// Close every mirror row an earlier daemon run left open (review
    /// WP75-R7): its relay state died with that run, so no decision on it
    /// can reach the desktop. Run once, before this run's first put.
    async fn sweep_stale(pool: &sqlx::SqlitePool) {
        let now = now_ms();
        let r = sqlx::query(
            "UPDATE shell_notifications SET resolved_at = ?, read_at = COALESCE(read_at, ?) \
             WHERE kind = 'permission' AND resolved_at IS NULL \
               AND dedupe_key LIKE 'permission:relay:%'",
        )
        .bind(now)
        .bind(now)
        .execute(pool)
        .await;
        match r {
            Ok(done) if done.rows_affected() > 0 => super::publish(ChangeReason::Read, None),
            Ok(_) => {}
            Err(e) => log::warn!(target: "ikenga::notifications", "relay sweep: {e}"),
        }
    }
}

/// Desktop boot (review WP75-RV2): the hook / ACP asks an earlier desktop
/// run left open. Their hold (the hook gate's parked response, the ACP
/// round-trip) died with that process, so none can still take a decision —
/// including a remote decision the relay delivered just before a crash and
/// the desktop never applied. Each is resolved here (the ask is over) and
/// its key returned, for the desktop to report to the daemon as
/// `permission_relay_resolve {outcome: 'cancelled'}` so the daemon retracts
/// a `permission.decided` it wrote for a decision that never took effect.
/// `before_ms` is this process's start: an ask raised since is live.
pub async fn sweep_orphaned_asks(pool: &sqlx::SqlitePool, before_ms: i64) -> Vec<String> {
    let rows: Vec<(i64, String)> = match sqlx::query_as(
        "SELECT id, dedupe_key FROM shell_notifications \
         WHERE kind = 'permission' AND resolved_at IS NULL AND created_at < ? \
           AND (dedupe_key LIKE 'permission:hook:%' OR dedupe_key LIKE 'permission:acp:%')",
    )
    .bind(before_ms)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            log::warn!(target: "ikenga::notifications", "orphaned-ask sweep: {e}");
            return Vec::new();
        }
    };
    let now = now_ms();
    let mut keys = Vec::with_capacity(rows.len());
    for (id, key) in rows {
        let r = sqlx::query(
            "UPDATE shell_notifications SET resolved_at = ?, read_at = COALESCE(read_at, ?) \
             WHERE id = ? AND resolved_at IS NULL",
        )
        .bind(now)
        .bind(now)
        .bind(id)
        .execute(pool)
        .await;
        match r {
            Ok(done) if done.rows_affected() > 0 => keys.push(key),
            Ok(_) => {}
            Err(e) => log::warn!(target: "ikenga::notifications", "orphaned ask {key}: {e}"),
        }
    }
    if !keys.is_empty() {
        super::publish(ChangeReason::Read, None);
    }
    keys
}

/// The daemon's resolver table: only the relay (§5.5).
pub struct RelayResolvers(pub Arc<Relay>);

impl AskResolvers for RelayResolvers {
    fn resolve<'a>(
        &'a self,
        key: &'a AskKey,
        decision: Decision,
        by: &'a DecidedBy,
    ) -> Option<BoxFuture<'a, Result<(), AccessError>>> {
        match key {
            AskKey::Relay { key } => {
                let r = self.0.queue(key, decision, by);
                Some(Box::pin(async move { r }))
            }
            _ => None,
        }
    }
}

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, AccessError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AccessError::new(Code::InvalidRequest, format!("`{key}` is required")))
}

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

// ─── runtimes ───────────────────────────────────────────────────────────────

/// The T0 daemon's routing state: its `ikenga.db` (where the relay mirror
/// rows live), its access store (the audit chain) and the relay.
pub struct DaemonRouting {
    pub db: Arc<crate::db::PaDb>,
    pub store: AccessStore,
    pub relay: Arc<Relay>,
    /// The boot sweep of an earlier run's mirror rows ran (review WP75-R7).
    swept: tokio::sync::OnceCell<()>,
}

impl DaemonRouting {
    /// Over the daemon's own `ikenga.db` handle — the one `run_server`
    /// hands its router (`AppState.pa_db`), so the daemon keeps a single
    /// writer pool (review WP75-R9).
    pub fn new(store: AccessStore, db: Arc<crate::db::PaDb>) -> Self {
        DaemonRouting {
            db,
            relay: Relay::with_store(store.clone()),
            store,
            swept: tokio::sync::OnceCell::new(),
        }
    }

    /// The daemon's `ikenga.db` writer (the server's own `pa_db`, which
    /// opens and migrates on first use), with an earlier run's relay rows
    /// swept first.
    pub async fn pool(&self) -> Result<sqlx::SqlitePool, AccessError> {
        let pool = self.db.ensure_pool().await.map_err(AccessError::internal)?;
        self.swept.get_or_init(|| Relay::sweep_stale(&pool)).await;
        Ok(pool)
    }
}

static DAEMON: OnceLock<DaemonRouting> = OnceLock::new();

/// Installed once by the T0 daemon's access boot (`access::DaemonAccess::
/// boot_t0`), with `run_server`'s own `pa_db` (the `<data-dir>/ikenga.db`
/// the daemon serves) — never a second `PaDb` on the same file.
pub fn install_daemon(rt: DaemonRouting) {
    let _ = DAEMON.set(rt);
}

pub fn daemon() -> Option<&'static DaemonRouting> {
    DAEMON.get()
}

/// The desktop side (§5.5): what `permission_decide` reaches in-process.
pub trait HostSide: Send + Sync {
    /// `routing_ok` for the host device (§5.1): the desktop asks the daemon
    /// for the preference. `Err` fails closed.
    fn host_routing(&self) -> BoxFuture<'_, Result<HostRouting, AccessError>>;
    /// `access_audit_record_local {kind: permission.*}` (§6.5, §6.9).
    fn audit_local(&self, kind: &'static str, target: String, detail: Value) -> BoxFuture<'_, ()>;
}

/// The host's routing view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostRouting {
    pub routing_ok: bool,
    pub owner: Option<String>,
    pub host_device: Option<String>,
}

pub struct LocalRouting {
    pub db: Arc<crate::db::PaDb>,
    pub resolvers: Arc<dyn AskResolvers>,
    pub host: Arc<dyn HostSide>,
}

static LOCAL: OnceLock<LocalRouting> = OnceLock::new();

/// Installed once by the desktop (`crate::notifications`).
pub fn install_local(local: LocalRouting) {
    let _ = LOCAL.set(local);
}

pub fn local() -> Option<&'static LocalRouting> {
    LOCAL.get()
}

// ─── RPC entry points ───────────────────────────────────────────────────────

/// `permission_decide`'s core over the daemon runtime (T0 daemon, T1
/// child). Kept for the §9.2 skeleton name.
pub async fn decide(
    ctx: &AccessCtx,
    notification_id: i64,
    decision: &str,
) -> Result<Value, AccessError> {
    let Some(rt) = daemon() else {
        // A T1 child: no daemon engine raises asks yet (§5.5 (c)), so no
        // row can name one here.
        return Err(AccessError::new(Code::NotFound, "no such ask"));
    };
    let pool = rt.pool().await?;
    decide_on_relay(&pool, &rt.relay, ctx, notification_id, decision).await
}

/// The daemon's decide over its relay: `decide_with`, then the audit — a
/// remote decision's `permission.decided` goes through the relay, which
/// can still retract it if the desktop never applies it (review WP75-R2).
pub async fn decide_on_relay(
    pool: &sqlx::SqlitePool,
    relay: &Arc<Relay>,
    ctx: &AccessCtx,
    notification_id: i64,
    decision: &str,
) -> Result<Value, AccessError> {
    let audit = match &relay.store {
        Some(store) => AuditTo::Store(store),
        None => AuditTo::Off,
    };
    decide_on(
        pool,
        &RelayResolvers(relay.clone()),
        Some(relay),
        audit,
        ctx,
        notification_id,
        decision,
    )
    .await
}

/// Where a decision's audit row is written (daemon asks, gap 2).
pub enum AuditTo<'a> {
    /// The chain itself: the T0 daemon's store (or the T1 broker's).
    Store(&'a AccessStore),
    /// A T1 principal child, which holds no store: the note is queued for the
    /// broker (the chain's one writer) to validate and append
    /// ([`crate::access::audit::child`]). Queued, not written.
    Outbox(&'a crate::access::audit::child::Outbox),
    /// No store to write to (a router built without one).
    Off,
}

/// [`decide_with`] over any resolver table, then the audit. The relay is
/// only for a mirror row's decision (retractable, review WP75-R2); a hook
/// ask has none and is audited where it was decided.
pub async fn decide_on(
    pool: &sqlx::SqlitePool,
    resolvers: &dyn AskResolvers,
    relay: Option<&Arc<Relay>>,
    audit: AuditTo<'_>,
    ctx: &AccessCtx,
    notification_id: i64,
    decision: &str,
) -> Result<Value, AccessError> {
    let report = decide_with(
        pool,
        &Decider::from_ctx(ctx),
        notification_id,
        decision,
        resolvers,
    )
    .await;
    match audit {
        AuditTo::Store(store) => {
            if let Some(ev) = audit_event_for(Some(ctx), &report) {
                let relayed = match (&report.result, report.row.as_ref(), relay) {
                    (Ok(()), Some(row), Some(_)) => {
                        match AskKey::parse(row.dedupe_key.as_deref().unwrap_or("")) {
                            AskKey::Relay { key } => Some(key),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                match (relayed, relay) {
                    (Some(key), Some(relay)) => relay.audit_decided(&key, ev).await,
                    _ => append_audit(store, &ev).await,
                }
            }
        }
        AuditTo::Outbox(outbox) => {
            if let Some(note) = note_for(ctx, &report) {
                outbox.push(note);
            }
        }
        AuditTo::Off => {}
    }
    report.into_value()
}

/// The [`crate::access::audit::child::Note`] a T1 child queues for a report:
/// the same facts [`audit_event_for`] puts in the chain row, as the closed
/// record the broker will validate, with the deciding credential the broker
/// handed this child.
pub fn note_for(
    ctx: &AccessCtx,
    report: &DecideReport,
) -> Option<crate::access::audit::child::Note> {
    use crate::access::audit::child::{Note, NoteBy, NoteKind};
    let row = report.row.as_ref();
    let by = Decider::from_ctx(ctx).by;
    let (kind, reason) = match (&report.result, report.refused) {
        (Ok(()), _) => (NoteKind::Decided, None),
        (Err(_), Some(reason)) => (NoteKind::Refused, Some(reason.to_string())),
        (Err(_), None) => return None,
    };
    Some(Note {
        kind: Some(kind),
        target: row.map(|r| r.title.chars().take(120).collect()),
        decision: if kind == NoteKind::Decided {
            report.decision.map(|d| d.as_str().to_string())
        } else {
            None
        },
        outcome: None,
        reason,
        sensitive: row.map(|r| r.sensitivity.level()).unwrap_or(0),
        requested_by: row.and_then(|r| r.requested_by.clone()),
        project_key: match (&ctx.share, row.and_then(|r| r.project_id.as_ref())) {
            (Some(s), _) => Some(s.project_key.clone()),
            (None, Some(p)) => Some(format!("{}/{p}", ctx.principal_id)),
            (None, None) => None,
        },
        by: Some(NoteBy {
            principal_id: by.principal_id,
            via: by.via.to_string(),
            device_id: by.device_id,
        }),
    })
}

/// The desktop's in-process `permission_decide` (§5.5, review C-05): the
/// operator deciding its own hook / ACP asks against the desktop's
/// `ikenga.db`, capped by the host's routing preference (§5.1), audited
/// through `access_audit_record_local`.
pub async fn decide_local(notification_id: i64, decision: &str) -> Result<Value, AccessError> {
    let Some(rt) = local() else {
        return Err(AccessError::new(
            Code::Internal,
            "permission routing is not running in this process",
        ));
    };
    let routing = rt.host.host_routing().await?;
    let d = Decider::host(routing.routing_ok, routing.owner, routing.host_device);
    let pool = rt.db.ensure_pool().await.map_err(AccessError::internal)?;
    let report = decide_with(&pool, &d, notification_id, decision, rt.resolvers.as_ref()).await;
    if let Some(ev) = audit_event_for(None, &report) {
        rt.host
            .audit_local(
                ev.kind,
                ev.target.clone().unwrap_or_default(),
                ev.detail.clone(),
            )
            .await;
    }
    report.into_value()
}

/// The `permission_decide` RPC arm.
pub async fn decide_rpc(ctx: &AccessCtx, args: &Value) -> Result<Value, AccessError> {
    let id = args
        .get("notificationId")
        .and_then(Value::as_i64)
        .ok_or_else(|| AccessError::new(Code::InvalidRequest, "`notificationId` is required"))?;
    let decision = args.get("decision").and_then(Value::as_str).unwrap_or("");
    decide(ctx, id, decision).await
}

/// `permission_relay_put` / `_take` / `_resolve` (operator only; the
/// pre-hook already checked the class).
pub async fn relay_rpc(ctx: &AccessCtx, cmd: &str, args: &Value) -> Result<Value, AccessError> {
    if !ctx.is_operator() {
        return Err(AccessError::class(crate::access::ArmClass::Operator));
    }
    let Some(rt) = daemon() else {
        return Err(AccessError::store_unavailable());
    };
    match cmd {
        "permission_relay_take" => {
            let wait = args
                .get("waitMs")
                .and_then(Value::as_u64)
                .unwrap_or(RELAY_MAX_WAIT_MS);
            Ok(rt.relay.take(wait).await)
        }
        "permission_relay_put" => {
            let pool = rt.pool().await?;
            rt.relay.put(&pool, args).await
        }
        "permission_relay_resolve" => {
            let pool = rt.pool().await?;
            rt.relay.resolve(&pool, args).await
        }
        other => Err(AccessError::new(
            Code::NotFound,
            format!("no relay command `{other}`"),
        )),
    }
}

/// A refused `permission_decide` that never reached the arm (the pre-hook
/// refused it for `approve`, §5.4 step 1): audit `permission.refused`
/// (A-23). Daemon only; best effort.
pub async fn audit_prehook_refusal(ctx: &AccessCtx, cmd: &str, err: &AccessError) {
    if let (Some(ev), Some(rt)) = (prehook_refusal_event(ctx, cmd, err), daemon()) {
        append_audit(&rt.store, &ev).await;
    }
}

/// The `permission.refused` row for a `permission_decide` refused before it
/// reached the arm, or `None` (another command, or a refusal that is not
/// about `approve`). The one copy both writers use: the T0 daemon's
/// pre-hook ([`audit_prehook_refusal`]) and the T1 broker's
/// `authorize_rpc` (`access::t1::AccessAuthorizer`).
pub fn prehook_refusal_event(ctx: &AccessCtx, cmd: &str, err: &AccessError) -> Option<Event> {
    if cmd != "permission_decide" {
        return None;
    }
    let reason = match err.code {
        Code::RoutingRefused => "routing_refused",
        Code::Forbidden if err.message.contains("approve") => "forbidden",
        _ => return None,
    };
    let mut ev = Event::by("permission.refused", ctx).detail(json!({ "reason": reason }));
    ev.project_key = ctx.share.as_ref().map(|s| s.project_key.clone());
    Some(ev)
}

// ─── the read model (§5.7) ──────────────────────────────────────────────────

fn row_from_json(v: &Value) -> Option<AskRow> {
    if v.get("kind").and_then(Value::as_str) != Some("permission") {
        return None;
    }
    let routing = v.get("action").and_then(|a| a.get("routing"));
    Some(AskRow {
        id: v.get("id").and_then(Value::as_i64)?,
        dedupe_key: v
            .get("dedupeKey")
            .and_then(Value::as_str)
            .map(str::to_string),
        title: v
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        resolved_at: v.get("resolvedAt").and_then(Value::as_i64),
        project_id: routing
            .and_then(|r| r.get("projectId"))
            .and_then(Value::as_str)
            .map(str::to_string),
        requested_by: routing
            .and_then(|r| r.get("requestedBy"))
            .and_then(Value::as_str)
            .map(str::to_string),
        // No recorded classification (a row from before WP-75) fails closed.
        sensitivity: Sensitivity::from_level(
            routing
                .and_then(|r| r.get("sensitive"))
                .and_then(Value::as_i64)
                .unwrap_or(2),
        ),
    })
}

/// Annotate `permission` rows with `can_decide` / `waiting_on` for this
/// request (§5.7). `rows` is `notifications_list`'s data (an array).
pub fn annotate(ctx: &AccessCtx, rows: &mut Value) {
    match daemon() {
        Some(rt) => annotate_with(ctx, rows, &|key| rt.relay.is_open(key)),
        None => annotate_with(ctx, rows, &|_| true),
    }
}

/// [`annotate`] with an explicit "is this relay mirror live in this daemon
/// run" check: an open mirror row whose ask the relay no longer holds (an
/// earlier run's, review WP75-R7) reads as over.
pub fn annotate_with(ctx: &AccessCtx, rows: &mut Value, relay_live: &dyn Fn(&str) -> bool) {
    let d = Decider::from_ctx(ctx);
    let Some(list) = rows.as_array_mut() else {
        return;
    };
    for v in list.iter_mut() {
        let Some(mut row) = row_from_json(v) else {
            continue;
        };
        let key = AskKey::parse(row.dedupe_key.as_deref().unwrap_or(""));
        if let AskKey::Relay { key } = &key {
            if row.resolved_at.is_none() && !relay_live(key) {
                row.resolved_at = Some(0);
            }
        }
        let answerable = matches!(
            key,
            AskKey::Hook { .. } | AskKey::Acp { .. } | AskKey::Relay { .. }
        );
        let verdict = can_decide(&d, &row, None);
        let (can, waiting) = match verdict {
            Ok(()) => (answerable, None),
            Err(r) => (false, waiting_on(r)),
        };
        if let Value::Object(map) = v {
            map.insert("can_decide".into(), Value::Bool(can));
            map.insert(
                "waiting_on".into(),
                waiting.map_or(Value::Null, |w| json!(w)),
            );
            // "Always for this project" is offered only where the engine has
            // one and the decider may persist a rule (§5.4 3d, §5.5).
            let always = can
                && key.offers_always()
                && can_decide(&d, &row, Some(Decision::AllowAlwaysProject)).is_ok();
            map.insert("can_allow_always".into(), Value::Bool(always));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{RequestMeta, Role};
    use crate::executor::PrincipalId;

    // ── A-22: the classifier table ──

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn classifier_table() {
        let root = Some("/work/royalti-co");
        let cases: &[(&str, Value, Option<&str>, Sensitivity)] = &[
            (
                "Read",
                json!({"file_path": "src/main.rs"}),
                root,
                Sensitivity::NONE,
            ),
            (
                "Read",
                json!({"file_path": "/work/royalti-co/a.md"}),
                root,
                Sensitivity::NONE,
            ),
            (
                "Edit",
                json!({"file_path": "../other/x.rs"}),
                root,
                Sensitivity::SENSITIVE,
            ),
            (
                "Write",
                json!({"file_path": "/etc/hosts"}),
                root,
                Sensitivity::SENSITIVE,
            ),
            (
                "Read",
                json!({"file_path": "~/notes.md"}),
                root,
                Sensitivity::SENSITIVE,
            ),
            (
                "Glob",
                json!({"pattern": "**/*.rs", "path": "src"}),
                root,
                Sensitivity::NONE,
            ),
            (
                "Grep",
                json!({"paths": ["src", "/tmp/x"]}),
                root,
                Sensitivity::SENSITIVE,
            ),
            (
                "NotebookEdit",
                json!({"notebook_path": "nb/a.ipynb"}),
                root,
                Sensitivity::NONE,
            ),
            (
                "Read",
                json!({"file_path": "src/main.rs"}),
                None,
                Sensitivity::SENSITIVE,
            ),
            (
                "WebSearch",
                json!({"query": "x"}),
                None,
                Sensitivity::SENSITIVE,
            ),
            ("WebSearch", json!({"query": "x"}), root, Sensitivity::NONE),
            (
                "Bash",
                json!({"command": "cargo check"}),
                root,
                Sensitivity::SENSITIVE,
            ),
            ("BashOutput", json!({}), root, Sensitivity::SENSITIVE),
            ("KillShell", json!({}), root, Sensitivity::SENSITIVE),
            ("KillBash", json!({}), root, Sensitivity::SENSITIVE),
            ("mcp__box__exec", json!({}), root, Sensitivity::SENSITIVE),
            ("mcp__box__shell", json!({}), root, Sensitivity::SENSITIVE),
            (
                "mcp__box__run_command",
                json!({}),
                root,
                Sensitivity::SENSITIVE,
            ),
            ("mcp__box__execute", json!({}), root, Sensitivity::NONE),
            (
                "Read",
                json!({"file_path": ".env"}),
                root,
                Sensitivity::SECRET,
            ),
            (
                "Read",
                json!({"file_path": "config/.env.local"}),
                root,
                Sensitivity::SECRET,
            ),
            (
                "Read",
                json!({"file_path": "certs/server.pem"}),
                root,
                Sensitivity::SECRET,
            ),
            (
                "Read",
                json!({"file_path": "tls/server.key"}),
                root,
                Sensitivity::SECRET,
            ),
            (
                "Read",
                json!({"file_path": "/home/u/.ssh/config"}),
                root,
                Sensitivity::SECRET,
            ),
            (
                "Read",
                json!({"file_path": "keys/id_ed25519"}),
                root,
                Sensitivity::SECRET,
            ),
            (
                "Read",
                json!({"file_path": "/home/u/.aws/credentials"}),
                root,
                Sensitivity::SECRET,
            ),
            (
                "Read",
                json!({"file_path": "/home/u/.gnupg/pubring.kbx"}),
                root,
                Sensitivity::SECRET,
            ),
            (
                "Read",
                json!({"file_path": "/home/u/.netrc"}),
                root,
                Sensitivity::SECRET,
            ),
            (
                "Read",
                json!({"file_path": "/home/u/.ikenga/secrets.json"}),
                root,
                Sensitivity::SECRET,
            ),
            (
                "Bash",
                json!({"command": "cat .env | head"}),
                root,
                Sensitivity::SECRET,
            ),
            (
                "Bash",
                json!({"command": "scp -i ~/.ssh/id_rsa a b:"}),
                root,
                Sensitivity::SECRET,
            ),
            (
                "Bash",
                json!({"command": "echo $IKENGA_SECRET_STRIPE"}),
                root,
                Sensitivity::SECRET,
            ),
            (
                "Write",
                json!({"file_path": "a.txt", "content": "IKENGA_SECRET_X"}),
                root,
                Sensitivity::SECRET,
            ),
            (
                "Bash",
                json!({"command": "ls environment/"}),
                root,
                Sensitivity::SENSITIVE,
            ),
            // Shell quoting can't split a name apart (review WP75-R8).
            (
                "Bash",
                json!({"command": "cat .e\"nv\""}),
                root,
                Sensitivity::SECRET,
            ),
            (
                "Bash",
                json!({"command": "cat '.e'nv.local"}),
                root,
                Sensitivity::SECRET,
            ),
            (
                "Bash",
                json!({"command": "cat ~/.s\\sh/config"}),
                root,
                Sensitivity::SECRET,
            ),
        ];
        for (tool, input, root, want) in cases {
            assert_eq!(
                classify_with(tool, input, *root, &none),
                *want,
                "{tool} {input} root={root:?}"
            );
        }
        // Rule 3, third bullet: an MCP tool of a pkg that declares secrets.
        let vault = |t: &str| mcp_server_of(t) == Some("pkg-vault-cms");
        assert_eq!(
            classify_with("mcp__pkg-vault-cms__list", &json!({}), root, &vault),
            Sensitivity::SECRET
        );
        assert_eq!(
            classify_with("mcp__pkg-other__list", &json!({}), root, &vault),
            Sensitivity::NONE
        );
        // A terminal PermissionRequest is shell exec (rule 1).
        assert_eq!(
            classify_terminal("Read", &json!({"file_path": "a"}), root),
            Sensitivity::SENSITIVE
        );
        assert_eq!(Sensitivity::from_level(7), Sensitivity::SECRET);
        for s in [
            Sensitivity::NONE,
            Sensitivity::SENSITIVE,
            Sensitivity::SECRET,
        ] {
            assert_eq!(Sensitivity::from_level(s.level()), s);
        }
    }

    /// §5.3 rules 2 and 3 follow symlinks on the host that records the ask
    /// (review WP75-R1): an in-project link to `~/.aws` or to `.env` is
    /// secret material, a link out of the project is outside it, and a link
    /// that stays inside stays NONE.
    #[cfg(unix)]
    #[test]
    fn classifier_follows_symlinks() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let home = base.join("home");
        let proj = base.join("proj");
        let elsewhere = base.join("elsewhere");
        std::fs::create_dir_all(home.join(".aws")).unwrap();
        std::fs::write(home.join(".aws/credentials"), "k").unwrap();
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(proj.join(".env"), "S=1").unwrap();
        std::fs::write(proj.join("src/a.rs"), "").unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("notes.md"), "").unwrap();
        symlink(home.join(".aws"), proj.join("config")).unwrap();
        symlink(proj.join(".env"), proj.join("settings")).unwrap();
        symlink(&elsewhere, proj.join("docs")).unwrap();
        symlink(proj.join("src"), proj.join("lib")).unwrap();

        let root = proj.to_string_lossy().to_string();
        let root = Some(root.as_str());
        let abs = |rel: &str| proj.join(rel).to_string_lossy().to_string();
        let cases: &[(&str, Value, Sensitivity)] = &[
            (
                "Read",
                json!({"file_path": abs("config/credentials")}),
                Sensitivity::SECRET,
            ),
            (
                "Read",
                json!({"file_path": "config/credentials"}),
                Sensitivity::SECRET,
            ),
            (
                "Read",
                json!({"file_path": "settings"}),
                Sensitivity::SECRET,
            ),
            (
                "Read",
                json!({"file_path": abs("settings")}),
                Sensitivity::SECRET,
            ),
            (
                "Bash",
                json!({"command": "cat settings"}),
                Sensitivity::SECRET,
            ),
            (
                "Read",
                json!({"file_path": "docs/notes.md"}),
                Sensitivity::SENSITIVE,
            ),
            (
                "Write",
                json!({"file_path": "docs/new.md"}),
                Sensitivity::SENSITIVE,
            ),
            ("Read", json!({"file_path": "lib/a.rs"}), Sensitivity::NONE),
            ("Read", json!({"file_path": "src/a.rs"}), Sensitivity::NONE),
            (
                "Write",
                json!({"file_path": "src/new.rs"}),
                Sensitivity::NONE,
            ),
        ];
        for (tool, input, want) in cases {
            assert_eq!(
                classify_with(tool, input, root, &none),
                *want,
                "{tool} {input}"
            );
        }
        // A root reached through a symlink classifies the same way.
        let alias = base.join("alias");
        symlink(&proj, &alias).unwrap();
        let alias_root = alias.to_string_lossy().to_string();
        assert_eq!(
            classify_with(
                "Read",
                &json!({"file_path": "src/a.rs"}),
                Some(&alias_root),
                &none
            ),
            Sensitivity::NONE
        );
        assert_eq!(
            classify_with(
                "Read",
                &json!({"file_path": "config/credentials"}),
                Some(&alias_root),
                &none
            ),
            Sensitivity::SECRET
        );
    }

    #[test]
    fn ask_keys_parse() {
        assert_eq!(
            AskKey::parse("permission:hook:perm-1-2"),
            AskKey::Hook {
                request_id: "perm-1-2".into()
            }
        );
        assert_eq!(
            AskKey::parse("permission:acp:t-1:req:9"),
            AskKey::Acp {
                thread_id: "t-1".into(),
                request_id: "req:9".into()
            }
        );
        assert_eq!(
            AskKey::parse("permission:relay:permission:acp:t:r"),
            AskKey::Relay {
                key: "permission:acp:t:r".into()
            }
        );
        assert_eq!(AskKey::parse("permission:terminal:abc"), AskKey::Terminal);
        assert_eq!(AskKey::parse("violation:x"), AskKey::Other);
        assert!(AskKey::parse("permission:relay:permission:acp:t:r").offers_always());
        assert!(!AskKey::parse("permission:relay:permission:hook:x").offers_always());
    }

    // ── fixtures ──

    async fn db() -> (tempfile::TempDir, sqlx::SqlitePool) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::db::PaDb::new(tmp.path().join("ikenga.db"))
            .ensure_pool()
            .await
            .unwrap();
        (tmp, pool)
    }

    fn ask(key: &str) -> NewNotification {
        NewNotification {
            kind: super::super::NotificationKind::Permission,
            title: "Claude wants to use Bash".into(),
            body: None,
            action: Some(json!({"kind": "permission.decide"})),
            source: "test".into(),
            dedupe_key: Some(key.into()),
            coalesce: Coalesce::Once,
        }
    }

    /// Records every call; answers per `gone`.
    #[derive(Default)]
    struct Spy {
        calls: Mutex<Vec<(AskKey, Decision, DecidedBy)>>,
        gone: bool,
    }

    impl AskResolvers for Spy {
        fn resolve<'a>(
            &'a self,
            key: &'a AskKey,
            decision: Decision,
            by: &'a DecidedBy,
        ) -> Option<BoxFuture<'a, Result<(), AccessError>>> {
            if matches!(key, AskKey::Other | AskKey::Terminal) {
                return None;
            }
            self.calls
                .lock()
                .unwrap()
                .push((key.clone(), decision, by.clone()));
            let gone = self.gone;
            Some(Box::pin(async move {
                if gone {
                    Err(AccessError::new(Code::Conflict, "timed out"))
                } else {
                    Ok(())
                }
            }))
        }
    }

    fn owner_device(tier: Tier, routing_ok: bool) -> AccessCtx {
        let caps = crate::access::caps::effective(
            crate::access::caps::RoleContext::OwnWorkspace,
            tier,
            CapSet::ALL,
            routing_ok,
        );
        AccessCtx {
            principal_id: PrincipalId::new_v7(),
            via: Via::Device {
                device_id: "phone".into(),
            },
            device_id: Some("phone".into()),
            tier,
            share: None,
            share_headers: false,
            caps,
            admin_strength: tier == Tier::Full,
            meta: RequestMeta {
                routing_withheld_approve: crate::access::ctx::routing_withheld_approve(
                    crate::access::caps::RoleContext::OwnWorkspace,
                    tier,
                    CapSet::ALL,
                    routing_ok,
                ),
                ..Default::default()
            },
        }
    }

    /// A member's relayed request in the Owner's T1 child (§4.5.3): caps
    /// from the broker, share from the headers.
    fn member(role: Role, tier: Tier, owner_approval: bool) -> AccessCtx {
        let caps = crate::access::caps::effective(
            crate::access::caps::RoleContext::Share {
                role,
                row: role.default_caps(),
                artifact_scope: false,
            },
            tier,
            CapSet::ALL,
            true,
        );
        AccessCtx {
            principal_id: PrincipalId::new_v7(),
            via: Via::Relayed,
            device_id: None,
            tier,
            share: Some(ShareCtx {
                project_key: "owner/royalti-co".into(),
                project_id: "royalti-co".into(),
                member_principal_id: Some("ada".into()),
                member_device_id: Some("ada-phone".into()),
                role: Some(role),
                artifact_path: None,
                owner_approval,
            }),
            share_headers: true,
            caps,
            admin_strength: false,
            meta: RequestMeta::default(),
        }
    }

    const FIXTURE_SOCKET: u64 = 0;

    /// Review WP75-R11: two sockets on one thread keep their own contexts;
    /// one closing leaves the other's; disagreeing live contexts fail
    /// closed (no member, no project, no root).
    #[test]
    fn prompt_contexts_are_per_socket() {
        let (owner, member) = (prompt_socket(), prompt_socket());
        let own = PromptContext {
            requested_by: None,
            project_id: Some("royalti-co".into()),
            project_root: Some("/srv/owner/royalti-co".into()),
        };
        let ada = PromptContext {
            requested_by: Some("ada".into()),
            ..own.clone()
        };
        set_prompt_context("th-sock", owner, own.clone());
        assert_eq!(prompt_context("th-sock"), Some(own.clone()));
        set_prompt_context("th-sock", member, ada.clone());
        assert_eq!(prompt_context("th-sock"), Some(PromptContext::default()));
        clear_prompt_context("th-sock", owner);
        assert_eq!(
            prompt_context("th-sock"),
            Some(ada.clone()),
            "the member's survives"
        );
        set_prompt_context("th-sock", member, ada.clone());
        assert_eq!(prompt_context("th-sock"), Some(ada));
        clear_prompt_context("th-sock", member);
        assert_eq!(prompt_context("th-sock"), None);
    }

    /// The T1 fixture producer (DEC-83 / §5.5 (c)): an ask raised by a
    /// member's dispatch in the Owner's child, attributed the way `chat_ws`
    /// captures it at the handshake.
    async fn fixture_ask(
        pool: &sqlx::SqlitePool,
        thread: &str,
        key: &str,
        tool: &str,
        input: Value,
    ) -> i64 {
        set_prompt_context(
            thread,
            FIXTURE_SOCKET,
            PromptContext {
                requested_by: Some("ada".into()),
                project_id: Some("royalti-co".into()),
                project_root: Some("/srv/owner/royalti-co".into()),
            },
        );
        record_ask_for_thread(pool, thread, ask(key), tool, &input)
            .await
            .unwrap()
            .unwrap()
            .id
    }

    async fn row_cols(
        pool: &sqlx::SqlitePool,
        id: i64,
    ) -> (Option<i64>, Option<String>, Option<String>, Option<String>) {
        let r = sqlx::query(
            "SELECT resolved_at, decided_by, decided_via, decided_device FROM shell_notifications WHERE id = ?",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
        (
            r.get("resolved_at"),
            r.get("decided_by"),
            r.get("decided_via"),
            r.get("decided_device"),
        )
    }

    #[tokio::test]
    async fn record_ask_stores_attribution_columns_and_read_copy() {
        let (_t, pool) = db().await;
        let id = fixture_ask(
            &pool,
            "thread-attr",
            "permission:acp:thread-attr:r1",
            "Read",
            json!({"file_path": ".env"}),
        )
        .await;
        let r = load_ask(&pool, id).await.unwrap().unwrap();
        assert_eq!(r.requested_by.as_deref(), Some("ada"));
        assert_eq!(r.project_id.as_deref(), Some("royalti-co"));
        assert_eq!(r.sensitivity, Sensitivity::SECRET);
        let listed = super::super::list(&pool, &Default::default())
            .await
            .unwrap();
        let v = serde_json::to_value(&listed).unwrap();
        let routing = &v[0]["action"]["routing"];
        assert_eq!(routing["sensitive"], 2);
        assert_eq!(routing["requestedBy"], "ada");
        clear_prompt_context("thread-attr", FIXTURE_SOCKET);
        assert!(prompt_context("thread-attr").is_none());
    }

    /// A-22 / A-38 (T1, fixture producer): with owner approval on, a
    /// sensitive ask in a share is decided only by the Owner; a secret-
    /// material ask needs the Owner with the policy on **or off**; members
    /// never persist a rule.
    #[tokio::test]
    async fn share_routing_on_t1_with_a_fixture_producer() {
        let (_t, pool) = db().await;
        let spy = Spy::default();
        let plain = fixture_ask(
            &pool,
            "t1",
            "permission:acp:t1:a",
            "Read",
            json!({"file_path": "README.md"}),
        )
        .await;
        let bash = fixture_ask(
            &pool,
            "t1",
            "permission:acp:t1:b",
            "Bash",
            json!({"command": "ls"}),
        )
        .await;
        let secret = fixture_ask(
            &pool,
            "t1",
            "permission:acp:t1:c",
            "Read",
            json!({"file_path": ".env"}),
        )
        .await;

        let op_on = Decider::from_ctx(&member(Role::Operator, Tier::Approve, true));
        let op_off = Decider::from_ctx(&member(Role::Operator, Tier::Approve, false));

        // Sensitive + policy on → owner_approval_required, audited.
        let r = decide_with(&pool, &op_on, bash, "allow_once", &spy).await;
        assert_eq!(
            r.result.as_ref().unwrap_err().code,
            Code::OwnerApprovalRequired
        );
        assert_eq!(r.refused, Some("owner_approval_required"));
        // Secret material: refused with the policy on and off (3c).
        for d in [&op_on, &op_off] {
            let r = decide_with(&pool, d, secret, "deny", &spy).await;
            assert_eq!(r.result.unwrap_err().code, Code::OwnerApprovalRequired);
        }
        // Members never persist a rule (3d), even on a plain ask.
        let r = decide_with(&pool, &op_off, plain, "allow_always_project", &spy).await;
        let e = r.result.unwrap_err();
        assert_eq!(
            (e.code, e.message.starts_with("class=owner")),
            (Code::Forbidden, true)
        );
        assert!(
            spy.calls.lock().unwrap().is_empty(),
            "no refusal reaches a resolver"
        );

        // Policy off: the member may answer the sensitive Bash ask.
        let r = decide_with(&pool, &op_off, bash, "allow_once", &spy).await;
        assert!(r.result.is_ok(), "{:?}", r.result);
        let (resolved, by, via, device) = row_cols(&pool, bash).await;
        assert!(resolved.is_some());
        assert_eq!(
            (by.as_deref(), via.as_deref(), device.as_deref()),
            (Some("ada"), Some("device"), Some("ada-phone"))
        );
        // A plain ask under the policy: any approve-holding member.
        assert!(decide_with(&pool, &op_on, plain, "allow_once", &spy)
            .await
            .result
            .is_ok());

        // A Reviewer holds no approve: forbidden, not routing.
        let rev = Decider::from_ctx(&member(Role::Reviewer, Tier::Full, false));
        let r = decide_with(&pool, &rev, secret, "deny", &spy).await;
        assert_eq!(r.refused, Some("forbidden"));

        // The Owner (own workspace, a session) decides the secret ask, and
        // may persist a rule.
        let owner = Decider {
            by: DecidedBy {
                principal_id: Some("owner".into()),
                via: "session",
                device_id: None,
            },
            caps: CapSet::ALL,
            tier: Tier::Full,
            share: None,
            routing_withheld: false,
        };
        let r = decide_with(&pool, &owner, secret, "allow_always_project", &spy).await;
        assert!(r.result.is_ok(), "{:?}", r.result);
        let ev = audit_event_for(None, &r).unwrap();
        assert_eq!(ev.kind, "permission.decided");
        assert_eq!(ev.detail["sensitive"], 2);
        assert_eq!(ev.detail["requested_by"], "ada");
        // Already decided → conflict (step 2), never a second resolve.
        let r = decide_with(&pool, &owner, secret, "deny", &spy).await;
        assert_eq!(r.result.unwrap_err().code, Code::Conflict);
        assert_eq!(spy.calls.lock().unwrap().len(), 3);

        // 3a: an ask from another project is invisible to the share.
        let other = record_ask(
            &pool,
            ask("permission:acp:t9:z"),
            &Attribution {
                project_id: Some("other".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .unwrap()
        .id;
        let r = decide_with(&pool, &op_off, other, "deny", &spy).await;
        assert_eq!(r.result.unwrap_err().code, Code::NotFound);
        clear_prompt_context("t1", FIXTURE_SOCKET);
    }

    /// A-23: under `this_device`, any other credential is refused
    /// `routing_refused` (audited `permission.refused`); the named device
    /// decides.
    #[tokio::test]
    async fn this_device_routing_refuses_other_credentials() {
        let (_t, pool) = db().await;
        let spy = Spy::default();
        let id = record_ask(&pool, ask("permission:hook:h1"), &Attribution::default())
            .await
            .unwrap()
            .unwrap()
            .id;
        for d in [
            Decider::from_ctx(&owner_device(Tier::Approve, false)),
            Decider::from_ctx(&owner_device(Tier::Full, false)),
            Decider::host(false, Some("o".into()), Some("host".into())),
        ] {
            let r = decide_with(&pool, &d, id, "allow_once", &spy).await;
            assert_eq!(r.result.as_ref().unwrap_err().code, Code::RoutingRefused);
            assert_eq!(r.refused, Some("routing_refused"));
            let ev = audit_event_for(None, &r).unwrap();
            assert_eq!(
                (ev.kind, ev.detail["reason"].as_str()),
                ("permission.refused", Some("routing_refused"))
            );
        }
        // A dispatch-tier device was never able to approve: forbidden.
        let r = decide_with(
            &pool,
            &Decider::from_ctx(&owner_device(Tier::Dispatch, true)),
            id,
            "deny",
            &spy,
        )
        .await;
        assert_eq!(r.refused, Some("forbidden"));
        assert!(spy.calls.lock().unwrap().is_empty());
        // The named device: decided.
        let r = decide_with(
            &pool,
            &Decider::from_ctx(&owner_device(Tier::Approve, true)),
            id,
            "deny",
            &spy,
        )
        .await;
        assert!(r.result.is_ok());
        let calls = spy.calls.lock().unwrap();
        assert_eq!(calls[0].1, Decision::Deny);
        assert_eq!(calls[0].2.device_id.as_deref(), Some("phone"));
    }

    #[tokio::test]
    async fn decide_dispatches_on_the_key_prefix() {
        let (_t, pool) = db().await;
        let spy = Spy::default();
        let host = Decider::host(true, Some("o".into()), Some("host".into()));
        let term = record_ask(&pool, ask("permission:terminal:t"), &Attribution::default())
            .await
            .unwrap()
            .unwrap()
            .id;
        assert_eq!(
            decide_with(&pool, &host, term, "allow_once", &spy)
                .await
                .result
                .unwrap_err()
                .code,
            Code::AnswerInTerminal
        );
        let hook = record_ask(&pool, ask("permission:hook:h"), &Attribution::default())
            .await
            .unwrap()
            .unwrap()
            .id;
        // The hook gate has no "always" option.
        assert_eq!(
            decide_with(&pool, &host, hook, "allow_always_project", &spy)
                .await
                .result
                .unwrap_err()
                .code,
            Code::InvalidRequest
        );
        assert_eq!(
            decide_with(&pool, &host, hook, "maybe", &spy)
                .await
                .result
                .unwrap_err()
                .code,
            Code::InvalidRequest
        );
        // No resolver registered for the prefix → not_found; the row stays open.
        let r = decide_with(&pool, &host, hook, "allow_once", &NoResolvers).await;
        assert_eq!(r.result.unwrap_err().code, Code::NotFound);
        assert!(row_cols(&pool, hook).await.0.is_none());
        assert!(decide_with(&pool, &host, hook, "allow_once", &spy)
            .await
            .result
            .is_ok());
        let (_, by, via, device) = row_cols(&pool, hook).await;
        assert_eq!(
            (by.as_deref(), via.as_deref(), device.as_deref()),
            (Some("o"), Some("operator"), Some("host"))
        );
        // Not a permission row → not_found.
        assert_eq!(
            decide_with(&pool, &host, 999, "deny", &spy)
                .await
                .result
                .unwrap_err()
                .code,
            Code::NotFound
        );
    }

    /// A-24: an ask that times out under the decider ends resolved with no
    /// decision recorded; routing never re-routes or auto-allows.
    #[tokio::test]
    async fn a_timed_out_ask_never_becomes_a_decision() {
        let (_t, pool) = db().await;
        let gone = Spy {
            gone: true,
            ..Default::default()
        };
        let host = Decider::host(true, None, None);
        let id = record_ask(&pool, ask("permission:hook:late"), &Attribution::default())
            .await
            .unwrap()
            .unwrap()
            .id;
        let r = decide_with(&pool, &host, id, "allow_once", &gone).await;
        assert_eq!(r.result.unwrap_err().code, Code::Conflict);
        let (resolved, by, via, _) = row_cols(&pool, id).await;
        assert!(resolved.is_some(), "the ask is over");
        assert_eq!((by, via), (None, None), "no decision is attributed");
    }

    /// A-39 (in-process): a relay ask decided on a paired device queues one
    /// decision for the desktop, resolves the mirror, and keeps the daemon
    /// alive until it is taken; an ask the desktop resolves itself closes
    /// its mirror.
    #[tokio::test]
    async fn the_relay_round_trip() {
        let (_t, pool) = db().await;
        let relay = Relay::new();
        let put = |key: &str, ttl: i64| {
            json!({
                "key": key, "title": "Claude wants to use Edit", "body": "src/a.rs",
                "projectId": "royalti-co", "sensitive": 0, "expiresAtMs": now_ms() + ttl,
            })
        };
        let id = relay
            .put(&pool, &put("permission:acp:th:r1", 60_000))
            .await
            .unwrap()["id"]
            .as_i64()
            .unwrap();
        // Idempotent.
        let again = relay
            .put(&pool, &put("permission:acp:th:r1", 60_000))
            .await
            .unwrap();
        assert_eq!(again["id"], id);
        assert_eq!(relay.pending_count(), 1);
        assert_eq!(relay.held(), 1);

        // Refused puts.
        for bad in [
            json!({"key": "permission:terminal:x", "title": "t", "expiresAtMs": now_ms() + 1000}),
            json!({"key": "permission:relay:x", "title": "t", "expiresAtMs": now_ms() + 1000}),
            json!({"key": "permission:hook:x", "title": "t", "expiresAtMs": now_ms() - 1}),
            json!({"key": "permission:hook:x", "expiresAtMs": now_ms() + 1000}),
        ] {
            assert_eq!(
                relay.put(&pool, &bad).await.unwrap_err().code,
                Code::InvalidRequest,
                "{bad}"
            );
        }

        // The listed mirror row offers "always" (ACP) to an approve device.
        let mut rows = serde_json::to_value(
            super::super::list(&pool, &Default::default())
                .await
                .unwrap(),
        )
        .unwrap();
        let phone = owner_device(Tier::Approve, true);
        annotate(&phone, &mut rows);
        assert_eq!(rows[0]["can_decide"], true);
        assert_eq!(rows[0]["can_allow_always"], true);
        assert_eq!(rows[0]["action"]["via"], "relay");

        // Decided on the phone: queued for the desktop, audited once (by the
        // caller of decide_with — the daemon's `decide`).
        let r = decide_with(
            &pool,
            &Decider::from_ctx(&phone),
            id,
            "allow_always_project",
            &RelayResolvers(relay.clone()),
        )
        .await;
        assert!(r.result.is_ok(), "{:?}", r.result);
        assert_eq!(relay.pending_count(), 0);
        assert_eq!(relay.held(), 1, "held until taken");
        let taken = relay.take(1000).await;
        assert_eq!(
            taken["decisions"],
            json!([{"key": "permission:acp:th:r1", "decision": "allow_always_project",
                    "decidedVia": "device", "decidedDevice": "phone",
                    "decidedBy": phone.principal_id.to_string()}])
        );
        assert_eq!(relay.held(), 0);
        // A second take waits, then answers empty.
        assert_eq!(relay.take(20).await["decisions"], json!([]));

        // The desktop answered an ask itself: the mirror closes, the phone
        // can no longer decide it.
        let id2 = relay
            .put(&pool, &put("permission:hook:h2", 30_000))
            .await
            .unwrap()["id"]
            .as_i64()
            .unwrap();
        relay
            .resolve(
                &pool,
                &json!({"key": "permission:hook:h2", "outcome": "decided_on_host"}),
            )
            .await
            .unwrap();
        assert_eq!(relay.pending_count(), 0);
        assert!(load_ask(&pool, id2)
            .await
            .unwrap()
            .unwrap()
            .resolved_at
            .is_some());
        let r = decide_with(
            &pool,
            &Decider::from_ctx(&phone),
            id2,
            "allow_once",
            &RelayResolvers(relay.clone()),
        )
        .await;
        assert_eq!(r.result.unwrap_err().code, Code::Conflict);
        assert_eq!(
            relay
                .resolve(&pool, &json!({"key": "k", "outcome": "nope"}))
                .await
                .unwrap_err()
                .code,
            Code::InvalidRequest
        );
        assert_eq!(relay.held(), 0);
    }

    /// A-24 / A-39: an unanswered relay ask times out on its own — the
    /// mirror closes, nothing is queued, and a late decision is a conflict.
    #[tokio::test]
    async fn an_unanswered_relay_ask_times_out_closed() {
        let (_t, pool) = db().await;
        let relay = Relay::new();
        let id = relay
            .put(&pool, &json!({"key": "permission:hook:slow", "title": "t", "sensitive": 1, "expiresAtMs": now_ms() + 50}))
            .await
            .unwrap()["id"]
            .as_i64()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(relay.pending_count(), 0);
        assert!(load_ask(&pool, id)
            .await
            .unwrap()
            .unwrap()
            .resolved_at
            .is_some());
        assert_eq!(relay.take(10).await["decisions"], json!([]));
        let phone = Decider::from_ctx(&owner_device(Tier::Approve, true));
        let r = decide_with(
            &pool,
            &phone,
            id,
            "allow_once",
            &RelayResolvers(relay.clone()),
        )
        .await;
        assert_eq!(r.result.unwrap_err().code, Code::Conflict);
    }

    async fn audit_rows(store: &AccessStore) -> Vec<(String, Value)> {
        sqlx::query("SELECT kind, detail FROM audit_events ORDER BY seq")
            .fetch_all(store.pool())
            .await
            .unwrap()
            .iter()
            .map(|r| {
                let detail: String = r.get("detail");
                (r.get("kind"), serde_json::from_str(&detail).unwrap())
            })
            .collect()
    }

    fn relay_put(key: &str, ttl_ms: i64) -> Value {
        json!({"key": key, "title": "Claude wants to use Edit", "sensitive": 0,
               "expiresAtMs": now_ms() + ttl_ms})
    }

    /// Review WP75-R2: a remote decision is audited once when it is made;
    /// if it never takes effect on the desktop — reported back
    /// (`permission_relay_resolve`), or never taken before the ask expired
    /// — the daemon follows it with `permission.refused {not_applied}` and
    /// clears the mirror's `decided_*`. One the desktop applied stands.
    #[tokio::test]
    async fn a_remote_decision_that_never_applies_is_retracted() {
        let (_t, pool) = db().await;
        let store = AccessStore::memory_t0().await;
        let relay = Relay::with_store(store.clone());
        let phone = owner_device(Tier::Approve, true);
        let id_of = |v: Value| v["id"].as_i64().unwrap();
        // Rows the store wrote for itself before this test's.
        let n0 = audit_rows(&store).await.len();

        // 1. Applied: taken, no report — the decision stands.
        let a = id_of(
            relay
                .put(&pool, &relay_put("permission:hook:a", 30_000))
                .await
                .unwrap(),
        );
        decide_on_relay(&pool, &relay, &phone, a, "allow_once")
            .await
            .unwrap();
        assert_eq!(
            relay.take(100).await["decisions"][0]["key"],
            "permission:hook:a"
        );
        let rows = audit_rows(&store).await;
        assert_eq!(rows.len(), n0 + 1);
        assert_eq!(rows[n0].0, "permission.decided");
        assert_eq!(row_cols(&pool, a).await.2.as_deref(), Some("device"));

        // 2. Taken, then the desktop reports it was too late.
        let b = id_of(
            relay
                .put(&pool, &relay_put("permission:hook:b", 30_000))
                .await
                .unwrap(),
        );
        decide_on_relay(&pool, &relay, &phone, b, "allow_once")
            .await
            .unwrap();
        relay.take(100).await;
        relay
            .resolve(
                &pool,
                &json!({"key": "permission:hook:b", "outcome": "timed_out"}),
            )
            .await
            .unwrap();
        let rows = audit_rows(&store).await;
        assert_eq!(rows.len(), n0 + 3);
        assert_eq!(rows[n0 + 2].0, "permission.refused");
        assert_eq!(rows[n0 + 2].1["reason"], "not_applied");
        assert_eq!(rows[n0 + 2].1["outcome"], "timed_out");
        assert_eq!(rows[n0 + 2].1["decision"], "allow_once");
        let (resolved, by, via, device) = row_cols(&pool, b).await;
        assert!(resolved.is_some(), "the ask is over");
        assert_eq!((by, via, device), (None, None, None), "no phantom decision");
        // A second report is a no-op.
        relay
            .resolve(
                &pool,
                &json!({"key": "permission:hook:b", "outcome": "cancelled"}),
            )
            .await
            .unwrap();
        assert_eq!(audit_rows(&store).await.len(), n0 + 3);

        // 3. Queued, never taken, the ask expires: the outbox entry (and
        // its keep-alive) goes with it, retracted.
        let c = id_of(
            relay
                .put(&pool, &relay_put("permission:hook:c", 150))
                .await
                .unwrap(),
        );
        decide_on_relay(&pool, &relay, &phone, c, "deny")
            .await
            .unwrap();
        assert_eq!(relay.held(), 1);
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(relay.held(), 0, "an expired decision holds nothing");
        assert_eq!(relay.take(10).await["decisions"], json!([]));
        let rows = audit_rows(&store).await;
        assert_eq!(rows.len(), n0 + 5);
        assert_eq!(rows[n0 + 4].0, "permission.refused");
        assert_eq!(rows[n0 + 4].1["outcome"], "expired");
        assert_eq!(row_cols(&pool, c).await.1, None);
    }

    /// Review WP75-R7: mirror rows an earlier daemon run left open are
    /// swept, and read as over until then.
    #[tokio::test]
    async fn an_earlier_runs_relay_rows_are_not_live() {
        let (_t, pool) = db().await;
        let stale = record_ask(
            &pool,
            ask("permission:relay:permission:hook:old"),
            &Attribution::default(),
        )
        .await
        .unwrap()
        .unwrap()
        .id;
        let relay = Relay::new();
        let fresh = relay
            .put(&pool, &relay_put("permission:hook:new", 30_000))
            .await
            .unwrap()["id"]
            .as_i64()
            .unwrap();
        let mut rows = serde_json::to_value(
            super::super::list(&pool, &Default::default())
                .await
                .unwrap(),
        )
        .unwrap();
        annotate_with(&owner_device(Tier::Full, true), &mut rows, &|k| {
            relay.is_open(k)
        });
        let by_id = |id: i64| {
            rows.as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == id)
                .unwrap()
                .clone()
        };
        assert_eq!(by_id(stale)["can_decide"], false);
        assert_eq!(by_id(stale)["waiting_on"], Value::Null);
        assert_eq!(by_id(fresh)["can_decide"], true);

        Relay::sweep_stale(&pool).await;
        assert!(load_ask(&pool, stale)
            .await
            .unwrap()
            .unwrap()
            .resolved_at
            .is_some());
    }

    /// The take wakes as soon as a decision is queued.
    #[tokio::test]
    async fn take_wakes_on_a_decision() {
        let (_t, pool) = db().await;
        let relay = Relay::new();
        let id = relay
            .put(&pool, &json!({"key": "permission:hook:w", "title": "t", "sensitive": 0, "expiresAtMs": now_ms() + 30_000}))
            .await
            .unwrap()["id"]
            .as_i64()
            .unwrap();
        let waiter = {
            let relay = relay.clone();
            tokio::spawn(async move { relay.take(10_000).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        let phone = Decider::from_ctx(&owner_device(Tier::Full, true));
        assert!(
            decide_with(&pool, &phone, id, "deny", &RelayResolvers(relay.clone()))
                .await
                .result
                .is_ok()
        );
        let got = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got["decisions"][0]["decision"], "deny");
    }

    /// A-39, the desktop half: a taken decision resolves the local ask once,
    /// attributed to the deciding device; a second copy is a no-op.
    #[tokio::test]
    async fn the_desktop_applies_a_relayed_decision_once() {
        let (_t, pool) = db().await;
        let spy = Spy::default();
        let id = record_ask(&pool, ask("permission:hook:r9"), &Attribution::default())
            .await
            .unwrap()
            .unwrap()
            .id;
        let d = json!({"key": "permission:hook:r9", "decision": "allow_once",
                       "decidedVia": "device", "decidedDevice": "phone"});
        assert!(apply_relayed(&pool, &spy, Some("owner".into()), &d)
            .await
            .unwrap());
        let (resolved, by, via, device) = row_cols(&pool, id).await;
        assert!(resolved.is_some());
        assert_eq!(
            (by.as_deref(), via.as_deref(), device.as_deref()),
            (Some("owner"), Some("device"), Some("phone"))
        );
        assert!(!apply_relayed(&pool, &spy, None, &d).await.unwrap());
        assert_eq!(spy.calls.lock().unwrap().len(), 1);
        let bad = json!({"key": "permission:relay:x", "decision": "deny"});
        assert_eq!(
            apply_relayed(&pool, &spy, None, &bad)
                .await
                .unwrap_err()
                .code,
            Code::InvalidRequest
        );
    }

    /// §5.7 read model: `can_decide` / `waiting_on` per credential.
    #[test]
    fn annotate_names_what_is_missing() {
        let row = |id: i64, key: &str, sens: i64, project: Option<&str>, resolved: bool| {
            json!({
                "id": id, "kind": "permission", "title": "t", "dedupeKey": key,
                "resolvedAt": if resolved { json!(1) } else { Value::Null },
                "action": {"kind": "permission.decide", "routing": {"sensitive": sens, "projectId": project}},
            })
        };
        let base = json!([
            row(1, "permission:relay:permission:hook:a", 0, Some("royalti-co"), false),
            row(2, "permission:relay:permission:acp:t:b", 1, Some("royalti-co"), false),
            row(3, "permission:relay:permission:hook:c", 2, Some("royalti-co"), false),
            row(4, "permission:relay:permission:hook:d", 0, Some("royalti-co"), true),
            {"id": 5, "kind": "update", "title": "u"},
            row(6, "permission:terminal:x", 1, Some("royalti-co"), false),
        ]);
        let pick =
            |rows: &Value, i: usize| (rows[i]["can_decide"].clone(), rows[i]["waiting_on"].clone());

        let mut rows = base.clone();
        annotate(&owner_device(Tier::Approve, true), &mut rows);
        assert_eq!(pick(&rows, 0), (json!(true), Value::Null));
        assert_eq!(pick(&rows, 2), (json!(true), Value::Null));
        assert_eq!(pick(&rows, 3), (json!(false), Value::Null), "resolved");
        assert!(rows[4].get("can_decide").is_none(), "only permission rows");
        assert_eq!(
            pick(&rows, 5),
            (json!(false), Value::Null),
            "terminal asks are open-only"
        );
        assert_eq!(rows[1]["can_allow_always"], true);
        assert_eq!(rows[0]["can_allow_always"], false);

        let mut rows = base.clone();
        annotate(&owner_device(Tier::Dispatch, true), &mut rows);
        assert_eq!(pick(&rows, 0), (json!(false), json!("approve")));

        let mut rows = base.clone();
        annotate(&owner_device(Tier::Approve, false), &mut rows);
        assert_eq!(pick(&rows, 0), (json!(false), json!("device")));

        let mut rows = base.clone();
        annotate(&member(Role::Operator, Tier::Approve, true), &mut rows);
        assert_eq!(pick(&rows, 0), (json!(true), Value::Null));
        assert_eq!(pick(&rows, 1), (json!(false), json!("owner")));
        assert_eq!(pick(&rows, 2), (json!(false), json!("owner")));
        assert_eq!(rows[0]["can_allow_always"], false, "members never persist");

        let mut rows = base;
        annotate(&member(Role::Operator, Tier::Approve, false), &mut rows);
        assert_eq!(pick(&rows, 1), (json!(true), Value::Null));
        assert_eq!(
            pick(&rows, 2),
            (json!(false), json!("owner")),
            "secrets still go to the Owner"
        );
    }

    /// A-23 / review WP75-R5: `routing_refused` only when routing removed
    /// `approve` — not when the tier, the role or a per-member override
    /// row did.
    #[test]
    fn routing_removal_is_told_apart_from_tier_and_role() {
        use crate::access::caps::RoleContext;
        use crate::access::ctx::routing_withheld_approve as withheld;
        let own = RoleContext::OwnWorkspace;
        assert!(withheld(own, Tier::Full, CapSet::ALL, false));
        assert!(!withheld(own, Tier::Full, CapSet::ALL, true));
        assert!(!withheld(own, Tier::Dispatch, CapSet::ALL, false));
        let share = |role: Role, row: CapSet| RoleContext::Share {
            role,
            row,
            artifact_scope: false,
        };
        let op = share(Role::Operator, Role::Operator.default_caps());
        assert!(withheld(op, Tier::Approve, CapSet::ALL, false));
        let rev = share(Role::Reviewer, Role::Reviewer.default_caps());
        assert!(!withheld(rev, Tier::Approve, CapSet::ALL, false));
        // The project's override row took approve away from Operators: a
        // plain `forbidden`, even with routing also withholding it.
        let overridden = share(
            Role::Operator,
            Role::Operator.default_caps().without(Cap::Approve),
        );
        assert!(!withheld(overridden, Tier::Approve, CapSet::ALL, false));
        // The share ceiling took it away: likewise.
        assert!(!withheld(
            op,
            Tier::Approve,
            CapSet::ALL.without(Cap::Approve),
            false
        ));

        // And the decide core reads the recorded bit.
        let mut ctx = member(Role::Operator, Tier::Approve, false);
        ctx.caps = ctx.caps.without(Cap::Approve);
        let row = AskRow {
            id: 1,
            dedupe_key: Some("permission:relay:permission:hook:a".into()),
            title: "t".into(),
            resolved_at: None,
            project_id: Some("royalti-co".into()),
            requested_by: None,
            sensitivity: Sensitivity::NONE,
        };
        assert_eq!(
            can_decide(&Decider::from_ctx(&ctx), &row, None),
            Err(Refusal::MissingApprove)
        );
        ctx.meta.routing_withheld_approve = true;
        assert_eq!(
            can_decide(&Decider::from_ctx(&ctx), &row, None),
            Err(Refusal::RoutingRefused)
        );
    }

    #[tokio::test]
    async fn project_for_path_takes_the_longest_root() {
        let (_t, pool) = db().await;
        for (id, root) in [("outer", "/work"), ("inner", "/work/royalti-co")] {
            sqlx::query("INSERT INTO projects (id, display_name, root_path, created_at) VALUES (?, ?, ?, 0)")
                .bind(id)
                .bind(id)
                .bind(root)
                .execute(&pool)
                .await
                .unwrap();
        }
        assert_eq!(
            project_for_path(&pool, "/work/royalti-co/src")
                .await
                .map(|p| p.0),
            Some("inner".into())
        );
        assert_eq!(
            project_for_path(&pool, "/work/x").await.map(|p| p.0),
            Some("outer".into())
        );
        assert_eq!(project_for_path(&pool, "/elsewhere").await, None);
    }

    /// Review WP75-R9: the daemon's routing runtime runs on the server's own
    /// `ikenga.db` handle — one writer pool, not a second `PaDb`.
    #[tokio::test]
    async fn the_daemon_runtime_shares_the_servers_db_handle() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::db::PaDb::new(tmp.path().join("ikenga.db")));
        let rt = DaemonRouting::new(AccessStore::memory_t0().await, db.clone());
        assert!(Arc::ptr_eq(&rt.db, &db));
        // The stale-mirror sweep runs on that handle, before first use.
        let pool = db.ensure_pool().await.unwrap();
        let stale = record_ask(
            &pool,
            ask("permission:relay:permission:hook:old"),
            &Attribution::default(),
        )
        .await
        .unwrap()
        .unwrap()
        .id;
        rt.pool().await.unwrap();
        assert!(load_ask(&pool, stale)
            .await
            .unwrap()
            .unwrap()
            .resolved_at
            .is_some());
    }

    /// Review WP75-RV2: a desktop that crashed after a relayed decision was
    /// delivered but before it applied leaves its ask open. The boot sweep
    /// closes it (and every other hook / ACP ask the dead run held, never a
    /// newer one), and its report retracts the daemon's `permission.decided`.
    #[tokio::test]
    async fn a_crash_between_take_and_apply_is_retracted_at_boot() {
        // The daemon: a mirror, decided on the phone, taken by the desktop.
        let (_dt, daemon_pool) = db().await;
        let store = AccessStore::memory_t0().await;
        let relay = Relay::with_store(store.clone());
        let n0 = audit_rows(&store).await.len();
        let mirror = relay
            .put(&daemon_pool, &relay_put("permission:hook:crash", 30_000))
            .await
            .unwrap()["id"]
            .as_i64()
            .unwrap();
        decide_on_relay(
            &daemon_pool,
            &relay,
            &owner_device(Tier::Approve, true),
            mirror,
            "allow_once",
        )
        .await
        .unwrap();
        assert_eq!(
            relay.take(100).await["decisions"][0]["key"],
            "permission:hook:crash"
        );

        // The desktop's own rows: the delivered ask, an ACP ask, a terminal
        // ask (answered in its terminal; not swept), and one raised after
        // this boot (live; not swept).
        let (_t, pool) = db().await;
        let mut ids = Vec::new();
        for key in [
            "permission:hook:crash",
            "permission:acp:t1:r1",
            "permission:terminal:x",
        ] {
            ids.push(
                record_ask(&pool, ask(key), &Attribution::default())
                    .await
                    .unwrap()
                    .unwrap()
                    .id,
            );
        }
        let booted = now_ms() + 1;
        tokio::time::sleep(Duration::from_millis(5)).await;
        let fresh = record_ask(&pool, ask("permission:hook:new"), &Attribution::default())
            .await
            .unwrap()
            .unwrap()
            .id;
        let mut keys = sweep_orphaned_asks(&pool, booted).await;
        keys.sort();
        assert_eq!(keys, ["permission:acp:t1:r1", "permission:hook:crash"]);
        for (id, swept) in [
            (ids[0], true),
            (ids[1], true),
            (ids[2], false),
            (fresh, false),
        ] {
            let resolved = load_ask(&pool, id).await.unwrap().unwrap().resolved_at;
            assert_eq!(resolved.is_some(), swept, "row {id}");
        }
        assert!(sweep_orphaned_asks(&pool, booted).await.is_empty(), "once");

        // The desktop reports each; the daemon retracts its claim.
        for key in &keys {
            relay
                .resolve(&daemon_pool, &json!({"key": key, "outcome": "cancelled"}))
                .await
                .unwrap();
        }
        let rows = audit_rows(&store).await;
        assert_eq!(rows.len(), n0 + 2);
        assert_eq!(rows[n0].0, "permission.decided");
        assert_eq!(rows[n0 + 1].0, "permission.refused");
        assert_eq!(rows[n0 + 1].1["reason"], "not_applied");
        assert_eq!(rows[n0 + 1].1["outcome"], "cancelled");
        assert_eq!(row_cols(&daemon_pool, mirror).await.1, None);
    }
}
