//! Outside file links (box-files WP-15; the design is
//! `plans/2026-11-20-box-files-sharing/06-file-links-security.md` in the
//! workspace meta-repo, cited below as "§N").
//!
//! An account publishes **one of its own files** to people with no account:
//! the broker mints a 256-bit token, stores only its SHA-256 in the access
//! store's `file_links` table (`migrations/0004_file_links.sql`), and hands
//! the creator the only copy inside the link's URL, `<public_url>/s/<token>`.
//!
//! This module is the store every later link WP trusts:
//!
//! * the token rule ([`mint_token`], [`token_hash`], [`looks_like_token`]);
//! * the preview-kind / mime table (§5.1, [`classify`]) and the file-name
//!   cleaning (§4.1, [`clean_name`]), shared by the broker and the child's
//!   `file_link_resolve` arm (`server::file_link_resolve`);
//! * the five broker arms (§7): `access_file_link_create`,
//!   `access_file_links_list`, `access_file_link_revoke` (owner) and
//!   `access_file_links_admin_list`, `access_file_link_admin_revoke`
//!   (admin).
//!
//! The arms run in the T1 broker, as root, and never touch a principal's
//! path themselves: a create asks the **creator's own child** to resolve the
//! path (`file_link_resolve`, class `internal`, through
//! [`OwnerCalls`](super::share::OwnerCalls)), so the account's own
//! `PathGuard` and unix permissions decide what may be linked.
//!
//! The whole feature sits behind `--file-links` / `IKENGA_FILE_LINKS`
//! ([`AccessOptions::file_links`], default off). Off, a create is refused;
//! listing and revoking keep working so existing links stay visible and
//! revocable. On T0 every arm answers `not_found: outside links need a
//! server`. The public `/s/*` routes are WP-16's, not this module's.
//!
//! The module is platform-neutral (the access layer builds on every OS);
//! only the passcode hash needs `server::operator`, which is Linux-only, and
//! is `cfg`-gated.

use base64::Engine;
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqliteRow;
use sqlx::{Connection, Row, SqliteConnection};

use super::audit::Event;
use super::caps::{Cap, CapSet};
use super::ctx::{AccessCtx, Via};
use super::policy::audit_err;
use super::rpc::Env;
use super::share::{broker_host, now_ms, OwnerCalls};
use super::store::{AccessStore, StoreTier};
use super::{AccessError, AccessOptions, Code};
use crate::executor::PrincipalId;

/// Characters in a token: 32 bytes, base64url without padding (§2.1).
pub const TOKEN_LEN: usize = 43;
/// The largest file a link may carry (§4.1 check 4, O-4).
pub const LINK_MAX_BYTES: u64 = 1 << 30;
/// An image larger than this is download-only (§5.1, O-4).
pub const IMAGE_PREVIEW_MAX_BYTES: u64 = 25 * 1024 * 1024;
/// A text file larger than this is download-only (§5.1, O-4).
pub const TEXT_PREVIEW_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Live links (not revoked, not expired) one account may hold (§3, O-4).
pub const MAX_LIVE_LINKS: i64 = 200;
/// Expiry when the creator names none (D-R1-8).
pub const DEFAULT_EXPIRY_DAYS: i64 = 7;
/// The longest expiry in days; longer means `never: true`.
pub const MAX_EXPIRY_DAYS: i64 = 365;
/// Passcode length bounds, in characters (§2.2).
pub const PASSCODE_MIN_CHARS: usize = 6;
pub const PASSCODE_MAX_CHARS: usize = 128;
/// The child arm a create asks to resolve the path (§4.1).
pub const RESOLVE_ARM: &str = "file_link_resolve";
/// The answer of every arm on a server without accounts.
pub const NEEDS_SERVER: &str = "outside links need a server";

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// How long an ended link stays in its owner's list (§7).
const ENDED_VISIBLE_MS: i64 = 30 * DAY_MS;
/// `file_links.name` CHECK (1..=255).
const MAX_NAME_CHARS: usize = 255;
/// `file_links.path` CHECK (2..=4096).
pub const MAX_PATH_BYTES: usize = 4096;
/// The most rows one admin listing returns.
const ADMIN_LIST_LIMIT: i64 = 5000;

// ─── token ──────────────────────────────────────────────────────────────────

/// A fresh token and its stored hash.
pub struct MintedToken {
    pub token: String,
    pub sha256: [u8; 32],
}

/// 32 bytes from the OS RNG, base64url without padding: 43 characters, no
/// prefix, no link id (§2.1).
pub fn mint_token() -> MintedToken {
    let mut secret = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut secret);
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret);
    let sha256 = token_hash(&token);
    MintedToken { token, sha256 }
}

/// `SHA-256(token bytes as typed)`: the only form at rest (§2.1). A fast
/// hash is right: the input carries 256 bits of entropy. Not trimmed: a
/// token with a stray space is malformed and fails [`looks_like_token`].
pub fn token_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// The shape check that runs before any DB work (§2.1): exactly 43 chars
/// of `[A-Za-z0-9_-]`.
pub fn looks_like_token(raw: &str) -> bool {
    raw.len() == TOKEN_LEN
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The link's URL: `<public_url>/s/<token>`.
pub fn link_url(base: &str, token: &str) -> String {
    format!("{}/s/{token}", base.trim().trim_end_matches('/'))
}

// ─── what may be shown ──────────────────────────────────────────────────────

/// How the public page previews a file (§5.1). Stored in
/// `file_links.preview_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    Pdf,
    Image,
    Text,
    /// Download only.
    None,
}

impl PreviewKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            PreviewKind::Pdf => "pdf",
            PreviewKind::Image => "image",
            PreviewKind::Text => "text",
            PreviewKind::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pdf" => PreviewKind::Pdf,
            "image" => PreviewKind::Image,
            "text" => PreviewKind::Text,
            "none" => PreviewKind::None,
            _ => return None,
        })
    }

    /// The `max` the broker passes for `/view` (§5.1); `None` for a
    /// download-only link, whose `/view` is the uniform 404.
    pub const fn preview_limit(self) -> Option<u64> {
        match self {
            PreviewKind::Pdf => Some(LINK_MAX_BYTES),
            PreviewKind::Image => Some(IMAGE_PREVIEW_MAX_BYTES),
            PreviewKind::Text => Some(TEXT_PREVIEW_MAX_BYTES),
            PreviewKind::None => None,
        }
    }
}

/// The `mime` a download-only link is stored and served as.
pub const OCTET_STREAM: &str = "application/octet-stream";

/// §5.1's table: the extension (case-insensitive) picks the kind and the
/// stored `mime`; everything else, **including** `.html .htm .svg .xml
/// .js`, is download-only `application/octet-stream`. A preview-kind file
/// over its preview limit is stored as `none`.
///
/// Mapped from the extension directly rather than through `mime_guess`, so
/// the result is exactly the table (`.md`, `.csv` and `.log` are text) and
/// can only ever name a type the `mime` CHECK allows.
pub fn classify(name: &str, size: u64) -> (PreviewKind, &'static str) {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let (kind, mime) = match ext.as_str() {
        "pdf" => (PreviewKind::Pdf, "application/pdf"),
        "png" => (PreviewKind::Image, "image/png"),
        "jpg" | "jpeg" => (PreviewKind::Image, "image/jpeg"),
        "gif" => (PreviewKind::Image, "image/gif"),
        "webp" => (PreviewKind::Image, "image/webp"),
        "txt" | "md" | "markdown" | "csv" | "log" => {
            (PreviewKind::Text, "text/plain; charset=utf-8")
        }
        _ => (PreviewKind::None, OCTET_STREAM),
    };
    match kind.preview_limit() {
        Some(limit) if size > limit => (PreviewKind::None, OCTET_STREAM),
        _ => (kind, mime),
    }
}

/// Whether `c` is a bidi embedding / override / isolate or an implicit
/// direction mark (§4.1 names U+202A..U+202E and U+2066..U+2069; the marks
/// U+200E, U+200F and U+061C are stripped too, conservatively).
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
    )
}

/// The name a link shows (§4.1): C0/C1 controls and bidi controls removed,
/// path separators replaced, trimmed, at most 255 characters. Never empty.
pub fn clean_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control() && !is_bidi_control(*c))
        .map(|c| if c == '/' || c == '\\' { '_' } else { c })
        .collect();
    let cleaned: String = cleaned.trim().chars().take(MAX_NAME_CHARS).collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "download".to_string()
    } else {
        cleaned
    }
}

/// A file a link may carry, as the owner's child resolved it (§4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// Canonical, absolute.
    pub path: String,
    pub name: String,
    pub size: u64,
    pub preview_kind: PreviewKind,
    pub mime: &'static str,
}

impl Resolved {
    /// From a canonical path, its raw basename and its size.
    pub fn from_parts(path: String, raw_name: &str, size: u64) -> Self {
        let name = clean_name(raw_name);
        let (preview_kind, mime) = classify(&name, size);
        Self {
            path,
            name,
            size,
            preview_kind,
            mime,
        }
    }

    /// The `file_link_resolve` answer.
    pub fn to_json(&self) -> Value {
        json!({
            "path": self.path,
            "name": self.name,
            "size": self.size,
            "mime": self.mime,
            "previewKind": self.preview_kind.as_str(),
        })
    }

    /// The broker's reading of the child's answer. The child runs as the
    /// account's uid, so nothing it says is taken on trust beyond what can
    /// only hurt that account: the path must be absolute and fit the column,
    /// the name is cleaned again, the size is re-checked, and the kind and
    /// mime are **recomputed** here from the name (the child's own `mime` /
    /// `previewKind` are ignored).
    pub fn from_child(v: &Value) -> Result<Self, AccessError> {
        let bad = || {
            AccessError::new(
                Code::Internal,
                "the workspace answered an unusable file description",
            )
        };
        let path = v.get("path").and_then(Value::as_str).ok_or_else(bad)?;
        if !path.starts_with('/')
            || path.len() < 2
            || path.len() > MAX_PATH_BYTES
            || path.contains('\0')
        {
            return Err(bad());
        }
        let raw_name = v.get("name").and_then(Value::as_str).ok_or_else(bad)?;
        let size = v.get("size").and_then(Value::as_u64).ok_or_else(bad)?;
        if size > LINK_MAX_BYTES {
            return Err(too_large());
        }
        Ok(Self::from_parts(path.to_string(), raw_name, size))
    }
}

/// The refusal for a file over [`LINK_MAX_BYTES`].
pub fn too_large() -> AccessError {
    AccessError::new(
        Code::NotFound,
        "files over 1 GiB can't be shared by an outside link",
    )
}

// ─── the gate and the rules ─────────────────────────────────────────────────

/// Whether outside links are on: a T1 broker with `--file-links`
/// (`access_status.fileLinksEnabled`, §7.1).
pub fn enabled(tier: StoreTier) -> bool {
    tier == StoreTier::T1 && broker_host().is_some_and(|h| h.options.file_links)
}

fn needs_server() -> AccessError {
    AccessError::new(Code::NotFound, NEEDS_SERVER)
}

fn invalid(m: impl Into<String>) -> AccessError {
    AccessError::new(Code::InvalidRequest, m)
}

fn no_such_link() -> AccessError {
    AccessError::new(Code::NotFound, "no such link")
}

/// Own workspace only (§7): a request carrying a share selection, parsed or
/// not, never creates, lists or revokes a link.
fn own_workspace(ctx: &AccessCtx) -> Result<(), AccessError> {
    if ctx.share.is_some() || ctx.share_headers {
        return Err(AccessError::new(
            Code::Forbidden,
            "outside links are made from your own workspace, not from a share",
        ));
    }
    Ok(())
}

/// The owner arms (§7): own workspace, `files`, admin strength (a password
/// session or a `full` device: publishing to outsiders is not something a
/// `view`-tier phone may do).
fn require_owner(ctx: &AccessCtx) -> Result<(), AccessError> {
    own_workspace(ctx)?;
    if !ctx.has(Cap::Files) {
        return Err(AccessError::missing(CapSet::of(&[Cap::Files])));
    }
    if !ctx.admin_strength {
        return Err(AccessError::forbidden_admin());
    }
    Ok(())
}

/// The admin arms (§7): `host_health::authorize`'s rule. An admin account
/// (`accounts.is_admin`, as the broker resolved it for this request), admin
/// strength, never a share.
fn require_admin(principal_is_admin: bool, ctx: &AccessCtx) -> Result<(), AccessError> {
    own_workspace(ctx)?;
    if !principal_is_admin {
        return Err(AccessError::new(
            Code::Forbidden,
            "every account's links are for administrators",
        ));
    }
    if !ctx.admin_strength {
        return Err(AccessError::forbidden_admin());
    }
    Ok(())
}

/// What a create asks for, validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSpec {
    pub path: String,
    /// `None` = never expires.
    pub expires_at: Option<i64>,
    pub passcode: Option<String>,
}

/// Parse `access_file_link_create`'s args (§7): `{path, expiresInDays?:
/// 1..=365, never?: true, passcode?: 6..=128 chars}`. A missing, null or
/// empty passcode means none.
pub fn create_spec(args: &Value, now: i64) -> Result<CreateSpec, AccessError> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("`path` is required"))?;
    if path.is_empty() || path.len() > MAX_PATH_BYTES || path.contains('\0') {
        return Err(invalid("path must be a file path"));
    }
    let never = match args.get("never") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(invalid("never must be true or false")),
    };
    let days = match args.get("expiresInDays") {
        None | Some(Value::Null) => None,
        Some(v) => match v.as_i64() {
            Some(d) if (1..=MAX_EXPIRY_DAYS).contains(&d) => Some(d),
            _ => {
                return Err(invalid(format!(
                    "expiresInDays must be a whole number from 1 to {MAX_EXPIRY_DAYS}"
                )))
            }
        },
    };
    if never && days.is_some() {
        return Err(invalid("give expiresInDays or never, not both"));
    }
    let expires_at = if never {
        None
    } else {
        Some(now + days.unwrap_or(DEFAULT_EXPIRY_DAYS) * DAY_MS)
    };
    let passcode = match args.get("passcode") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.is_empty() => None,
        Some(Value::String(s)) => {
            let n = s.chars().count();
            if !(PASSCODE_MIN_CHARS..=PASSCODE_MAX_CHARS).contains(&n) {
                return Err(invalid(format!(
                    "a passcode is {PASSCODE_MIN_CHARS} to {PASSCODE_MAX_CHARS} characters"
                )));
            }
            Some(s.clone())
        }
        Some(_) => return Err(invalid("passcode must be text")),
    };
    Ok(CreateSpec {
        path: path.to_string(),
        expires_at,
        passcode,
    })
}

/// A listed link's `state` (§7): `revoked`, `expired`, `locked`, else `live`.
pub fn link_state(
    revoked_at: Option<i64>,
    expires_at: Option<i64>,
    locked_until: Option<i64>,
    now: i64,
) -> &'static str {
    if revoked_at.is_some() {
        "revoked"
    } else if expires_at.is_some_and(|e| e <= now) {
        "expired"
    } else if locked_until.is_some_and(|l| l > now) {
        "locked"
    } else {
        "live"
    }
}

/// Hash a passcode with the login's Argon2id parameters (§2.2), on the
/// blocking pool. Linux-only like `server::operator`; T1, the only tier that
/// creates links, is Linux-only too.
async fn hash_passcode(passcode: String) -> Result<String, AccessError> {
    #[cfg(target_os = "linux")]
    {
        tokio::task::spawn_blocking(move || {
            crate::server::operator::password::hash_blocking(&passcode)
        })
        .await
        .map_err(AccessError::internal)?
        .map_err(AccessError::internal)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = passcode;
        Err(needs_server())
    }
}

async fn live_count(
    conn: &mut SqliteConnection,
    owner: &str,
    now: i64,
) -> Result<i64, AccessError> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM file_links WHERE owner_principal_id = ? \
           AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > ?)",
    )
    .bind(owner)
    .bind(now)
    .fetch_one(&mut *conn)
    .await
    .map_err(AccessError::internal)
}

fn at_cap() -> AccessError {
    AccessError::new(
        Code::Conflict,
        format!("you have {MAX_LIVE_LINKS} live links; revoke one before making another"),
    )
}

// ─── the arms ───────────────────────────────────────────────────────────────

/// The five §7 arms (`access::rpc::dispatch`).
pub async fn dispatch(
    env: &Env<'_>,
    ctx: &AccessCtx,
    cmd: &str,
    args: &Value,
) -> Result<Value, AccessError> {
    if env.tier == StoreTier::T0 {
        return Err(needs_server());
    }
    let store = env.store.ok_or_else(AccessError::store_unavailable)?;
    let now = now_ms();
    match cmd {
        "access_file_link_create" => {
            let host = broker_host().ok_or_else(|| {
                AccessError::new(Code::Internal, "the broker's link host is not wired")
            })?;
            create(store, ctx, &host.options, &*host.calls, args, now).await
        }
        "access_file_links_list" => list(store, ctx, now).await,
        "access_file_link_revoke" => revoke(store, ctx, args, now).await,
        "access_file_links_admin_list" => {
            admin_list(store, ctx, env.principal.is_admin, args, now).await
        }
        "access_file_link_admin_revoke" => {
            admin_revoke(store, ctx, env.principal.is_admin, args, now).await
        }
        other => Err(AccessError::new(
            Code::NotFound,
            format!("no access command `{other}`"),
        )),
    }
}

/// `access_file_link_create` (§7) → `{linkId, url, name, size, previewKind,
/// expiresAt, hasPasscode}`. `url` carries the only copy of the token.
pub async fn create(
    store: &AccessStore,
    ctx: &AccessCtx,
    options: &AccessOptions,
    calls: &dyn OwnerCalls,
    args: &Value,
    now: i64,
) -> Result<Value, AccessError> {
    require_owner(ctx)?;
    if !options.file_links {
        return Err(AccessError::new(
            Code::Forbidden,
            "outside links are turned off on this server",
        ));
    }
    let base = options
        .public_url
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .ok_or_else(|| {
            AccessError::new(
                Code::Forbidden,
                "outside links need the server's public address (--public-url)",
            )
        })?
        .to_string();
    let spec = create_spec(args, now)?;
    let owner = ctx.principal_id.to_string();
    {
        let mut conn = store
            .pool()
            .acquire()
            .await
            .map_err(AccessError::internal)?;
        if live_count(&mut conn, &owner, now).await? >= MAX_LIVE_LINKS {
            return Err(at_cap());
        }
    }
    // §4.1: the creator's own child resolves the path against its own
    // guard. No share header is sent, so it is the unnarrowed workspace.
    let answer = calls
        .call(ctx.principal_id, RESOLVE_ARM, json!({ "path": spec.path }))
        .await?;
    let file = Resolved::from_child(&answer)?;
    let passcode_phc = match spec.passcode {
        Some(p) => Some(hash_passcode(p).await?),
        None => None,
    };
    let (created_via, device_id) = match &ctx.via {
        Via::Device { device_id } => ("device", Some(device_id.clone())),
        _ => ("session", None),
    };
    let link_id = PrincipalId::new_v7().to_string();
    let minted = mint_token();

    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(AccessError::internal)?;
    // Again under the write lock: two creates racing at 199 can't both land.
    if live_count(&mut tx, &owner, now).await? >= MAX_LIVE_LINKS {
        return Err(at_cap());
    }
    sqlx::query(
        "INSERT INTO file_links (link_id, token_sha256, owner_principal_id, path, name, \
           preview_kind, mime, size_at_create, created_at, created_via, created_device_id, \
           expires_at, passcode_phc) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&link_id)
    .bind(minted.sha256.as_slice())
    .bind(&owner)
    .bind(&file.path)
    .bind(&file.name)
    .bind(file.preview_kind.as_str())
    .bind(file.mime)
    .bind(file.size as i64)
    .bind(now)
    .bind(created_via)
    .bind(&device_id)
    .bind(spec.expires_at)
    .bind(&passcode_phc)
    .execute(&mut *tx)
    .await
    .map_err(AccessError::internal)?;
    // `link.created` is a gated kind: a degraded chain refuses it and the
    // insert rolls back with it (§8.1).
    let ev = Event::by("link.created", ctx)
        .subject_principal(owner.clone())
        .target(link_id.clone())
        .detail(json!({
            "name": file.name,
            "previewKind": file.preview_kind.as_str(),
            "expiresAt": spec.expires_at,
            "passcode": passcode_phc.is_some(),
        }));
    let head = store
        .chain()
        .append(&mut tx, &ev)
        .await
        .map_err(audit_err)?;
    tx.commit().await.map_err(AccessError::internal)?;
    store.chain().committed(head);
    tracing::info!(link_id = %link_id, "file link created");
    Ok(json!({
        "linkId": link_id,
        "url": link_url(&base, &minted.token),
        "name": file.name,
        "size": file.size,
        "previewKind": file.preview_kind.as_str(),
        "expiresAt": spec.expires_at,
        "hasPasscode": passcode_phc.is_some(),
    }))
}

/// The columns a listing reads. Never the token hash or the passcode hash.
const LIST_COLS: &str = "l.link_id, l.owner_principal_id, l.name, l.path, l.preview_kind, \
    l.created_at, l.expires_at, l.revoked_at, l.revoked_reason, \
    (l.passcode_phc IS NOT NULL) AS has_passcode, l.passcode_locked_until, l.open_count, \
    l.download_count, l.first_opened_at, l.last_opened_at";

fn row_json(r: &SqliteRow, now: i64) -> Value {
    let revoked_at: Option<i64> = r.get("revoked_at");
    let expires_at: Option<i64> = r.get("expires_at");
    let locked_until: Option<i64> = r.get("passcode_locked_until");
    let has_passcode: i64 = r.get("has_passcode");
    json!({
        "linkId": r.get::<String, _>("link_id"),
        "name": r.get::<String, _>("name"),
        "path": r.get::<String, _>("path"),
        "previewKind": r.get::<String, _>("preview_kind"),
        "createdAt": r.get::<i64, _>("created_at"),
        "expiresAt": expires_at,
        "revokedAt": revoked_at,
        "revokedReason": r.get::<Option<String>, _>("revoked_reason"),
        "hasPasscode": has_passcode != 0,
        "lockedUntil": locked_until.filter(|l| *l > now),
        "openCount": r.get::<i64, _>("open_count"),
        "downloadCount": r.get::<i64, _>("download_count"),
        "firstOpenedAt": r.get::<Option<i64>, _>("first_opened_at"),
        "lastOpenedAt": r.get::<Option<i64>, _>("last_opened_at"),
        "state": link_state(revoked_at, expires_at, locked_until, now),
    })
}

/// `access_file_links_list {}` (§7): the caller's own links, live plus those
/// that ended in the last 30 days. Works with the setting off.
pub async fn list(store: &AccessStore, ctx: &AccessCtx, now: i64) -> Result<Value, AccessError> {
    require_owner(ctx)?;
    let cut = now - ENDED_VISIBLE_MS;
    let rows = sqlx::query(&format!(
        "SELECT {LIST_COLS} FROM file_links l WHERE l.owner_principal_id = ? \
           AND (l.revoked_at IS NULL OR l.revoked_at > ?) \
           AND (l.expires_at IS NULL OR l.expires_at > ?) \
         ORDER BY l.created_at DESC, l.link_id DESC"
    ))
    .bind(ctx.principal_id.to_string())
    .bind(cut)
    .bind(cut)
    .fetch_all(store.pool())
    .await
    .map_err(AccessError::internal)?;
    let links: Vec<Value> = rows.iter().map(|r| row_json(r, now)).collect();
    Ok(json!({ "links": links }))
}

fn revoked_event(ctx: &AccessCtx, owner: &str, link_id: &str, name: &str, by: &str) -> Event {
    Event::by("link.revoked", ctx)
        .subject_principal(owner.to_string())
        .target(link_id.to_string())
        .detail(json!({ "name": name, "by": by }))
}

/// `access_file_link_revoke {linkId}` (§7) → `{revoked: true}`. Another
/// account's id answers the same `not_found` as an unknown one; revoking an
/// already revoked link is a no-op.
pub async fn revoke(
    store: &AccessStore,
    ctx: &AccessCtx,
    args: &Value,
    now: i64,
) -> Result<Value, AccessError> {
    require_owner(ctx)?;
    let link_id = args
        .get("linkId")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("`linkId` is required"))?;
    let me = ctx.principal_id.to_string();
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(AccessError::internal)?;
    let row: Option<(String, Option<i64>)> = sqlx::query_as(
        "SELECT name, revoked_at FROM file_links WHERE link_id = ? AND owner_principal_id = ?",
    )
    .bind(link_id)
    .bind(&me)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AccessError::internal)?;
    let (name, revoked_at) = row.ok_or_else(no_such_link)?;
    if revoked_at.is_some() {
        return Ok(json!({ "revoked": true }));
    }
    sqlx::query(
        "UPDATE file_links SET revoked_at = ?, revoked_by = ?, revoked_reason = 'owner' \
         WHERE link_id = ? AND owner_principal_id = ? AND revoked_at IS NULL",
    )
    .bind(now)
    .bind(&me)
    .bind(link_id)
    .bind(&me)
    .execute(&mut *tx)
    .await
    .map_err(AccessError::internal)?;
    // Not a gated kind: revocation works while the chain is degraded (P-35).
    let head = store
        .chain()
        .append(&mut tx, &revoked_event(ctx, &me, link_id, &name, "owner"))
        .await
        .map_err(audit_err)?;
    tx.commit().await.map_err(AccessError::internal)?;
    store.chain().committed(head);
    tracing::info!(link_id = %link_id, "file link revoked by its owner");
    Ok(json!({ "revoked": true }))
}

fn owner_arg(args: &Value) -> Result<Option<String>, AccessError> {
    match args.get("ownerPrincipalId") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => s
            .parse::<PrincipalId>()
            .map(|p| Some(p.to_string()))
            .map_err(|_| invalid("ownerPrincipalId is not an account id")),
        Some(_) => Err(invalid("ownerPrincipalId is not an account id")),
    }
}

/// `access_file_links_admin_list {ownerPrincipalId?, includeEnded?}` (§7):
/// every account's links (or one account's), with `ownerUsername`. Without
/// `includeEnded`, only links that are neither revoked nor expired.
pub async fn admin_list(
    store: &AccessStore,
    ctx: &AccessCtx,
    principal_is_admin: bool,
    args: &Value,
    now: i64,
) -> Result<Value, AccessError> {
    require_admin(principal_is_admin, ctx)?;
    let owner = owner_arg(args)?;
    let include_ended = args
        .get("includeEnded")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let rows = sqlx::query(&format!(
        "SELECT {LIST_COLS}, a.username AS owner_username FROM file_links l \
           LEFT JOIN accounts a ON a.principal_id = l.owner_principal_id \
         WHERE (?1 IS NULL OR l.owner_principal_id = ?1) \
           AND (?2 = 1 OR (l.revoked_at IS NULL AND (l.expires_at IS NULL OR l.expires_at > ?3))) \
         ORDER BY l.created_at DESC, l.link_id DESC LIMIT ?4"
    ))
    .bind(&owner)
    .bind(include_ended as i64)
    .bind(now)
    .bind(ADMIN_LIST_LIMIT)
    .fetch_all(store.pool())
    .await
    .map_err(AccessError::internal)?;
    let links: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut v = row_json(r, now);
            v["ownerPrincipalId"] = json!(r.get::<String, _>("owner_principal_id"));
            v["ownerUsername"] = json!(r.get::<Option<String>, _>("owner_username"));
            v
        })
        .collect();
    Ok(json!({ "links": links }))
}

/// `access_file_link_admin_revoke {linkId} | {ownerPrincipalId}` (§7) →
/// `{revoked: n}`. The second form revokes every live link of one account
/// (incident response). `revoked_reason = 'admin'`, one `link.revoked` row
/// per link.
pub async fn admin_revoke(
    store: &AccessStore,
    ctx: &AccessCtx,
    principal_is_admin: bool,
    args: &Value,
    now: i64,
) -> Result<Value, AccessError> {
    require_admin(principal_is_admin, ctx)?;
    let link_id = args.get("linkId").and_then(Value::as_str);
    let owner = owner_arg(args)?;
    let me = ctx.principal_id.to_string();
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(AccessError::internal)?;
    let targets: Vec<(String, String, String)> = match (link_id, owner) {
        (Some(id), None) => {
            let row: Option<(String, String, Option<i64>)> = sqlx::query_as(
                "SELECT owner_principal_id, name, revoked_at FROM file_links WHERE link_id = ?",
            )
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(AccessError::internal)?;
            match row.ok_or_else(no_such_link)? {
                (_, _, Some(_)) => Vec::new(),
                (owner, name, None) => vec![(id.to_string(), owner, name)],
            }
        }
        (None, Some(owner)) => sqlx::query_as(
            "SELECT link_id, owner_principal_id, name FROM file_links \
             WHERE owner_principal_id = ? AND revoked_at IS NULL \
               AND (expires_at IS NULL OR expires_at > ?) ORDER BY created_at",
        )
        .bind(&owner)
        .bind(now)
        .fetch_all(&mut *tx)
        .await
        .map_err(AccessError::internal)?,
        _ => return Err(invalid("give linkId or ownerPrincipalId")),
    };
    let mut head = None;
    for (id, owner, name) in &targets {
        sqlx::query(
            "UPDATE file_links SET revoked_at = ?, revoked_by = ?, revoked_reason = 'admin' \
             WHERE link_id = ? AND revoked_at IS NULL",
        )
        .bind(now)
        .bind(&me)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(AccessError::internal)?;
        head = Some(
            store
                .chain()
                .append(&mut tx, &revoked_event(ctx, owner, id, name, "admin"))
                .await
                .map_err(audit_err)?,
        );
    }
    tx.commit().await.map_err(AccessError::internal)?;
    if let Some(head) = head {
        store.chain().committed(head);
    }
    tracing::info!(
        count = targets.len(),
        "file links revoked by an administrator"
    );
    Ok(json!({ "revoked": targets.len() }))
}

#[cfg(test)]
mod tests;
