//! Runtime resolver for sidecar / MCP child processes.
//!
//! Owns the location of the bundled `bun` binary (per ADR-010) and exposes
//! `resolve_command()` — the shim every spawn site should call before
//! `Command::new(...)`. A manifest's bare `"bun"` becomes the bundled binary;
//! anything else (absolute paths, `./bin/foo` shell-outs) passes through.
//!
//! Resolution order (B+A hybrid, per the 2026-06-02 runtime decision):
//!   1. Debug builds ONLY: `$CARGO_MANIFEST_DIR/resources/bun/<host-target>/bun`.
//!      Lets `tauri dev` exercise the bundled-Bun path without a release
//!      build, as long as `scripts/fetch-bun.sh --target <host>` has been
//!      run once. This branch is FIRST and unchanged so dev never fetches.
//!   2. `IKENGA_BUN_PATH` env override (any build) — a user-provided binary.
//!      Skips the sha-pin check by design (trusted, operator-supplied).
//!   3. A version-gated system `bun` on the augmented PATH (`>= BUN_VERSION`).
//!      Skips the sha-pin check by design (trusted PATH binary).
//!   4. A previously-fetched bun at `<app_data_dir>/bun/<target>/bun` whose
//!      `.zip-sha256` sidecar matches the pinned zip sha.
//!   5. None — the runtime must be fetched. `ensure_bun()` (run post-launch
//!      from `lib.rs`, never at `init_from_app`) downloads + sha-verifies +
//!      unzips the pinned bun release and seeds `FETCHED_BUN`.
//!
//! The release bundle no longer ships bun (`tauri.conf.json` dropped the
//! `resources/bun/**/*` line); a fresh install fetches it after the window
//! is interactive, and bun-dependent sidecars park as
//! `BlockedReason::RuntimeNotReady` (no strike) until the fetch completes.

use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};

/// Bun's release-asset target naming for the host we're building for. Maps
/// from Rust's `cfg(target_os/arch)` to the directory layout `fetch-bun.sh`
/// writes to. `unsupported` means we never wrote a binary for this host —
/// `bun_path()` will return None and `resolve_command` will fall through to
/// PATH lookup.
const BUN_TARGET: &str = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    "linux-x64"
} else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
    "linux-aarch64"
} else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
    "darwin-x64"
} else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    "darwin-aarch64"
} else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
    "windows-x64"
} else {
    "unsupported"
};

const BUN_BIN_NAME: &str = if cfg!(target_os = "windows") {
    "bun.exe"
} else {
    "bun"
};

// ── Pin source-of-truth ──────────────────────────────────────────────────────
//
// Mirrors `scripts/fetch-bun.sh`'s `BUN_VERSION` + `expected_sha_for()` case
// block. The `pin_table_matches_fetch_bun_script` test parses that script and
// asserts the two stay in lockstep — a bun bump must update BOTH or the test
// fails loudly. The per-target sha is the sha256 of the published `.zip`
// asset (not the unzipped binary), matching the sidecar contract.

/// Pinned Bun version (Bun's own `bun-vX.Y.Z` release tag, without the `bun-v`).
pub const BUN_VERSION: &str = "1.3.14";

/// Sha256 of `bun-<target>.zip` for each supported target. `None` for an
/// unsupported host (matches `BUN_TARGET == "unsupported"`).
pub fn bun_zip_sha256(target: &str) -> Option<&'static str> {
    match target {
        "linux-x64" => Some("951ee2aee855f08595aeec6225226a298d3fea83a3dcd6465c09cbccdf7e848f"),
        "linux-aarch64" => Some("a27ffb63a8310375836e0d6f668ae17fa8d8d18b88c37c821c65331973a19a3b"),
        "darwin-x64" => Some("4183df3374623e5bab315c547cfa0974533cd457d86b73b639f7a87974cd6633"),
        "darwin-aarch64" => Some("d8b96221828ad6f97ac7ac0ab7e95872341af763001e8803e8267652c2652620"),
        "windows-x64" => Some("0a0620930b6675d7ba440e81f4e0e00d3cfbe096c4b140d3fff02205e9e18922"),
        _ => None,
    }
}

static BUN_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Runtime fetched bun, seeded by `ensure_bun()` after a successful download.
/// `BUN_PATH` is a boot-time `OnceLock` and cannot be re-set, so a separate
/// `RwLock` carries the post-launch-fetched path. `resolve_command`,
/// `bun_ready`, and `bun_path`-style lookups consult this as a fallback.
static FETCHED_BUN: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Initialise the bundled-Bun lookup at app setup time. Idempotent.
pub fn init_from_app(app: &AppHandle) {
    let _ = BUN_PATH.get_or_init(|| resolve_bun_path(app));
}

/// Look for the bundled bun under the source tree (debug builds). Pulled out
/// of `resolve_bun_path` so unit tests can exercise it without an AppHandle.
#[cfg(debug_assertions)]
fn resolve_dev_bun_path() -> Option<PathBuf> {
    if BUN_TARGET == "unsupported" {
        return None;
    }
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources/bun")
        .join(BUN_TARGET)
        .join(BUN_BIN_NAME);
    if dev.is_file() {
        Some(dev)
    } else {
        None
    }
}

fn resolve_bun_path(app: &AppHandle) -> Option<PathBuf> {
    if BUN_TARGET == "unsupported" {
        log::warn!(
            "[runtime] bundled bun: host target unsupported by fetch-bun.sh — falling back to PATH"
        );
        return None;
    }

    // (1) Debug builds: prefer the source tree FIRST so `tauri dev` doesn't
    // need a bundle round-trip after `scripts/fetch-bun.sh` populates it, and
    // never triggers a fetch. This branch is unchanged.
    #[cfg(debug_assertions)]
    if let Some(dev) = resolve_dev_bun_path() {
        log::info!("[runtime] bundled bun (dev): {}", dev.display());
        return Some(dev);
    }

    // ── RELEASE resolve order (B+A hybrid) ────────────────────────────────

    // (1) IKENGA_BUN_PATH env override — user-supplied binary, skips sha check.
    if let Some(p) = std::env::var_os("IKENGA_BUN_PATH").map(PathBuf::from) {
        if p.is_file() {
            log::info!("[runtime] bun via IKENGA_BUN_PATH: {}", p.display());
            return Some(p);
        }
        log::warn!(
            "[runtime] IKENGA_BUN_PATH set but not a file: {} — ignoring",
            p.display()
        );
    }

    // (2) Version-gated system bun on the augmented PATH. Skips sha check —
    // a PATH/user-provided bun is trusted by design (the version gate is the
    // only guard). Accept `>= BUN_VERSION`.
    if let Ok(found) = which::which_in(
        "bun",
        Some(augmented_path()),
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    ) {
        if system_bun_version_ok(&found) {
            log::info!(
                "[runtime] system bun (>= {BUN_VERSION}): {}",
                found.display()
            );
            return Some(found);
        }
        log::info!(
            "[runtime] system bun at {} is older than pin {BUN_VERSION} (or unparsable) — skipping",
            found.display()
        );
    }

    // (3) Previously-fetched bun under app_data_dir, gated on the sidecar sha.
    if let Some(fetched) = fetched_bun_path(app) {
        if fetched.is_file() && fetched_sidecar_matches_pin(&fetched) {
            log::info!("[runtime] fetched bun: {}", fetched.display());
            return Some(fetched);
        }
    }

    // (4) Not resolved — `ensure_bun()` must fetch (post-launch).
    log::info!(
        "[runtime] no bun resolved at boot — will fetch bun-v{BUN_VERSION} ({BUN_TARGET}) after launch"
    );
    None
}

/// Run `<bun> --version` and accept only if `>= BUN_VERSION`. Any spawn or
/// parse failure returns false (treated as "not acceptable").
fn system_bun_version_ok(p: &Path) -> bool {
    let out = match std::process::Command::new(p).arg("--version").output() {
        Ok(o) => o,
        Err(_) => return false,
    };
    if !out.status.success() {
        return false;
    }
    let found = String::from_utf8_lossy(&out.stdout);
    version_ge(found.trim(), BUN_VERSION)
}

/// `found >= pin` by parsing `major.minor.patch` into a tuple. Pre-release /
/// build suffixes are ignored (only the leading `X.Y.Z` is parsed). Any parse
/// failure returns false.
fn version_ge(found: &str, pin: &str) -> bool {
    fn parse(v: &str) -> Option<(u32, u32, u32)> {
        // Tolerate a leading `v` and trailing `-canary`/`+build` noise.
        let core = v.trim().trim_start_matches('v');
        let core = core
            .split(|c: char| c == '-' || c == '+' || c == ' ')
            .next()
            .unwrap_or(core);
        let mut it = core.split('.');
        let maj = it.next()?.parse::<u32>().ok()?;
        let min = it.next()?.parse::<u32>().ok()?;
        let pat = it.next()?.parse::<u32>().ok()?;
        Some((maj, min, pat))
    }
    match (parse(found), parse(pin)) {
        (Some(f), Some(p)) => f >= p,
        _ => false,
    }
}

/// `<app_data_dir>/bun` — the root under which fetched bun targets live.
pub fn app_data_bun_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join("bun"))
}

/// Where a fetched bun for this host lands: `<app_data_dir>/bun/<target>/bun`.
/// `None` when the host target is unsupported.
pub fn fetched_bun_path(app: &AppHandle) -> Option<PathBuf> {
    if BUN_TARGET == "unsupported" {
        return None;
    }
    app_data_bun_dir(app).map(|d| d.join(BUN_TARGET).join(BUN_BIN_NAME))
}

/// Sidecar path for a fetched bun binary: `<bin>.zip-sha256`.
fn sidecar_path_for(bin: &Path) -> PathBuf {
    PathBuf::from(format!("{}.zip-sha256", bin.display()))
}

/// True iff the `.zip-sha256` sidecar next to `bin` holds the pinned zip sha
/// for the current target.
fn fetched_sidecar_matches_pin(bin: &Path) -> bool {
    let sidecar = sidecar_path_for(bin);
    match (std::fs::read_to_string(&sidecar), bun_zip_sha256(BUN_TARGET)) {
        (Ok(s), Some(pin)) => s.trim() == pin,
        _ => false,
    }
}

/// String-equality on the declared command — matches `resolve_command` and the
/// shell.execute gate's `"bun"` literal. Anything else (absolute paths,
/// `./bin/foo`) is not a bun command.
pub fn is_bun_command(declared: &str) -> bool {
    declared == "bun"
}

/// The post-launch-fetched bun path, if `ensure_bun()` has seeded it.
fn fetched_runtime() -> Option<PathBuf> {
    FETCHED_BUN.read().ok().and_then(|g| g.clone())
}

/// Whether a usable bun is resolvable — either boot-resolved (`BUN_PATH`) or
/// post-launch-fetched (`FETCHED_BUN`). Deferred sidecars gate on this.
pub fn bun_ready() -> bool {
    bun_path().is_some() || fetched_runtime().is_some()
}

/// Path to the bundled bun, if `init_from_app` has run AND a binary exists.
pub fn bun_path() -> Option<&'static Path> {
    BUN_PATH.get().and_then(|opt| opt.as_deref())
}

static AUGMENTED_PATH: OnceLock<OsString> = OnceLock::new();

/// `$PATH` augmented with well-known per-user bin dirs that GUI-launched
/// processes routinely miss — nvm/npm global bins, Homebrew, `~/.local/bin`,
/// `/snap/bin`, bun, cargo. Built once and cached.
///
/// The inherited `$PATH` entries come first so a user's explicit ordering
/// still wins; the extras are appended as fallbacks and only when the dir
/// actually exists.
///
/// Fixes the class of "installed but invisible" agent-detection
/// false-negatives where `gemini`/`codex` live under an nvm node `bin/` that
/// the app's inherited `$PATH` doesn't include (ADR-013 §Addendum Decision 2;
/// tuicommander resolves the same GUI-launch gap). Use via
/// `which::which_in(name, Some(augmented_path()), cwd)` for detection and
/// `cmd.env("PATH", augmented_path())` at engine spawn sites.
pub fn augmented_path() -> &'static OsStr {
    AUGMENTED_PATH.get_or_init(build_augmented_path).as_os_str()
}

fn build_augmented_path() -> OsString {
    let mut entries: Vec<PathBuf> = Vec::new();

    // Inherited PATH first — the user's explicit ordering wins.
    if let Some(path) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&path));
    }

    // Candidate per-user bin dirs to append as fallbacks.
    let mut extras: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        extras.push(home.join(".local/bin"));
        extras.push(home.join(".bun/bin"));
        extras.push(home.join(".cargo/bin"));
        extras.push(home.join(".npm-global/bin"));
        // nvm installs CLIs under the active node version's bin/. Add every
        // installed version's bin so the agent is found regardless of which
        // node version `npm i -g` landed it under.
        if let Ok(rd) = std::fs::read_dir(home.join(".nvm/versions/node")) {
            for ent in rd.flatten() {
                extras.push(ent.path().join("bin"));
            }
        }
    }
    for p in [
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/local/bin",
        "/usr/local/sbin",
        "/snap/bin",
    ] {
        extras.push(PathBuf::from(p));
    }
    #[cfg(windows)]
    {
        if let Some(userprofile) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
            extras.push(userprofile.join(".cargo").join("bin"));
            extras.push(userprofile.join(".bun").join("bin"));
            extras.push(userprofile.join("scoop").join("shims"));
            extras.push(userprofile.join("AppData").join("Roaming").join("npm"));
            extras.push(userprofile.join("AppData").join("Local").join("pnpm"));
            extras.push(userprofile.join("AppData").join("Local").join("Programs").join("Git").join("bin"));
            extras.push(userprofile.join("AppData").join("Local").join("Programs").join("Git").join("cmd"));
            extras.push(userprofile.join("AppData").join("Local").join("Microsoft").join("WinGet").join("Links"));
        }
        if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
            extras.push(appdata.join("npm"));
        }
        if let Some(localappdata) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            extras.push(localappdata.join("pnpm"));
            extras.push(localappdata.join("Programs").join("Git").join("bin"));
            extras.push(localappdata.join("Programs").join("Git").join("cmd"));
            extras.push(localappdata.join("Microsoft").join("WinGet").join("Links"));
            extras.push(localappdata.join("Microsoft").join("WindowsApps"));
        }
        if let Some(progfiles) = std::env::var_os("ProgramFiles").map(PathBuf::from) {
            extras.push(progfiles.join("PowerShell").join("7"));
            extras.push(progfiles.join("Git").join("bin"));
            extras.push(progfiles.join("Git").join("cmd"));
            extras.push(progfiles.join("nodejs"));
        }
    }

    for e in extras {
        if e.is_dir() && !entries.iter().any(|x| x == &e) {
            entries.push(e);
        }
    }

    std::env::join_paths(entries).unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
}

/// Rewrite a manifest's `command` for spawn. Bare `"bun"` becomes the bundled
/// binary; absolute paths and `./bin/foo` shell-outs pass through unchanged.
pub fn resolve_command(declared: &str) -> PathBuf {
    if is_bun_command(declared) {
        if let Some(p) = bun_path() {
            return p.to_path_buf();
        }
        // Boot didn't resolve bun, but a post-launch fetch may have seeded it.
        if let Some(p) = fetched_runtime() {
            return p;
        }
    }
    PathBuf::from(declared)
}

// ── Post-launch fetcher (ensure_bun) ─────────────────────────────────────────

/// Progress narration for the bun fetch. Emitted on `runtime://bun` via the
/// `on_progress` closure threaded from `lib.rs` / the retry command. The wire
/// shape is hand-rolled in `to_payload()` (not a Serialize derive) so the
/// `downloading` pct can be omitted when content-length is unknown.
#[derive(Debug, Clone)]
pub enum BunFetchProgress {
    Checking,
    Downloading { pct: u8 },
    Verifying,
    Ready,
    Error { msg: String },
}

impl BunFetchProgress {
    pub fn to_payload(&self) -> serde_json::Value {
        match self {
            BunFetchProgress::Checking => serde_json::json!({ "state": "checking" }),
            BunFetchProgress::Downloading { pct } => {
                serde_json::json!({ "state": "downloading", "pct": pct })
            }
            BunFetchProgress::Verifying => serde_json::json!({ "state": "verifying" }),
            BunFetchProgress::Ready => serde_json::json!({ "state": "ready" }),
            BunFetchProgress::Error { msg } => {
                serde_json::json!({ "state": "error", "msg": msg })
            }
        }
    }
}

/// Lowercase hex of a byte slice. Avoids pulling in the `hex` crate's encode
/// at this site (sha2's GenericArray doesn't impl `LowerHex`); cheap + local.
fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Download + sha-verify + unzip the pinned bun release into
/// `<app_data_dir>/bun/<target>/`, chmod +x (unix), write the `.zip-sha256`
/// sidecar, and seed `FETCHED_BUN`. Emits progress via `on_progress`.
///
/// NEVER unzips an unverified download: a sha mismatch aborts before the unzip
/// step. Returns the path to the installed binary on success.
///
/// Idempotent: if bun already resolves (`bun_ready`) or a valid fetched binary
/// already sits on disk with a matching sidecar, returns early without
/// re-downloading.
pub async fn ensure_bun<F>(app: &AppHandle, on_progress: F) -> Result<PathBuf, String>
where
    F: Fn(BunFetchProgress) + Send + Sync,
{
    // Already resolvable (boot-resolved or previously fetched this session).
    if let Some(p) = bun_path() {
        return Ok(p.to_path_buf());
    }
    if let Some(p) = fetched_runtime() {
        return Ok(p);
    }

    on_progress(BunFetchProgress::Checking);

    if BUN_TARGET == "unsupported" {
        let msg = "host target unsupported by the bun runtime fetcher".to_string();
        on_progress(BunFetchProgress::Error { msg: msg.clone() });
        return Err(msg);
    }
    let expected_sha = match bun_zip_sha256(BUN_TARGET) {
        Some(s) => s,
        None => {
            let msg = format!("no pinned sha for target {BUN_TARGET}");
            on_progress(BunFetchProgress::Error { msg: msg.clone() });
            return Err(msg);
        }
    };

    let dir = match app_data_bun_dir(app) {
        Some(d) => d.join(BUN_TARGET),
        None => {
            let msg = "app_data_dir unavailable".to_string();
            on_progress(BunFetchProgress::Error { msg: msg.clone() });
            return Err(msg);
        }
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        let msg = format!("create_dir_all {}: {e}", dir.display());
        on_progress(BunFetchProgress::Error { msg: msg.clone() });
        return Err(msg);
    }
    let bin = dir.join(BUN_BIN_NAME);
    let sidecar = sidecar_path_for(&bin);

    // Fast-path: a valid cached binary with a matching sidecar — seed + done.
    if bin.is_file() && fetched_sidecar_matches_pin(&bin) {
        seed_fetched(&bin);
        on_progress(BunFetchProgress::Ready);
        return Ok(bin);
    }

    // ── Download (streamed, with pct when content-length is known) ──────────
    on_progress(BunFetchProgress::Downloading { pct: 0 });
    let url = format!(
        "https://github.com/oven-sh/bun/releases/download/bun-v{BUN_VERSION}/bun-{BUN_TARGET}.zip"
    );
    let resp = match reqwest::Client::new().get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("download request failed: {e}");
            on_progress(BunFetchProgress::Error { msg: msg.clone() });
            return Err(msg);
        }
    };
    let resp = match resp.error_for_status() {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("download HTTP error: {e}");
            on_progress(BunFetchProgress::Error { msg: msg.clone() });
            return Err(msg);
        }
    };
    let total = resp.content_length();
    let mut bytes: Vec<u8> = match total {
        Some(t) => Vec::with_capacity(t as usize),
        None => Vec::new(),
    };
    {
        use futures_util::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut downloaded: u64 = 0;
        let mut last_pct: u8 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let msg = format!("download stream error: {e}");
                    on_progress(BunFetchProgress::Error { msg: msg.clone() });
                    return Err(msg);
                }
            };
            downloaded += chunk.len() as u64;
            bytes.extend_from_slice(&chunk);
            if let Some(t) = total {
                if t > 0 {
                    let pct = ((downloaded.saturating_mul(100)) / t).min(100) as u8;
                    if pct != last_pct {
                        last_pct = pct;
                        on_progress(BunFetchProgress::Downloading { pct });
                    }
                }
            }
        }
    }

    // ── Verify (sha256 of the zip vs the pin) — abort before unzip on miss ──
    on_progress(BunFetchProgress::Verifying);
    let got = {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        hex_lower(&hasher.finalize())
    };
    if got != expected_sha {
        let msg = format!(
            "sha256 mismatch for bun-{BUN_TARGET}.zip (expected {expected_sha}, got {got})"
        );
        on_progress(BunFetchProgress::Error { msg: msg.clone() });
        return Err(msg);
    }

    // ── Unzip (only after verification passes) ──────────────────────────────
    let mut zip = match zip::ZipArchive::new(std::io::Cursor::new(&bytes)) {
        Ok(z) => z,
        Err(e) => {
            let msg = format!("open zip: {e}");
            on_progress(BunFetchProgress::Error { msg: msg.clone() });
            return Err(msg);
        }
    };
    let mut wrote = false;
    for i in 0..zip.len() {
        let mut entry = match zip.by_index(i) {
            Ok(e) => e,
            Err(e) => {
                let msg = format!("read zip entry {i}: {e}");
                on_progress(BunFetchProgress::Error { msg: msg.clone() });
                return Err(msg);
            }
        };
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_string();
        let tail = name.rsplit('/').next().unwrap_or(&name);
        if tail == BUN_BIN_NAME {
            let mut out = match std::fs::File::create(&bin) {
                Ok(f) => f,
                Err(e) => {
                    let msg = format!("create {}: {e}", bin.display());
                    on_progress(BunFetchProgress::Error { msg: msg.clone() });
                    return Err(msg);
                }
            };
            if let Err(e) = std::io::copy(&mut entry, &mut out) {
                let msg = format!("write bun binary: {e}");
                on_progress(BunFetchProgress::Error { msg: msg.clone() });
                return Err(msg);
            }
            if let Err(e) = out.flush() {
                let msg = format!("flush bun binary: {e}");
                on_progress(BunFetchProgress::Error { msg: msg.clone() });
                return Err(msg);
            }
            wrote = true;
            break;
        }
    }
    if !wrote {
        let msg = format!("no `{BUN_BIN_NAME}` found inside bun-{BUN_TARGET}.zip");
        on_progress(BunFetchProgress::Error { msg: msg.clone() });
        return Err(msg);
    }

    // chmod +x on unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)) {
            let msg = format!("chmod +x {}: {e}", bin.display());
            on_progress(BunFetchProgress::Error { msg: msg.clone() });
            return Err(msg);
        }
    }

    // Sidecar — records the verified zip sha so future boots trust this binary
    // without re-downloading.
    if let Err(e) = std::fs::write(&sidecar, format!("{expected_sha}\n")) {
        let msg = format!("write sidecar {}: {e}", sidecar.display());
        on_progress(BunFetchProgress::Error { msg: msg.clone() });
        return Err(msg);
    }

    seed_fetched(&bin);
    on_progress(BunFetchProgress::Ready);
    Ok(bin)
}

/// Seed `FETCHED_BUN` so `resolve_command` / `bun_ready` pick up the binary on
/// the deferred sidecars' next spawn.
fn seed_fetched(bin: &Path) {
    if let Ok(mut g) = FETCHED_BUN.write() {
        *g = Some(bin.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_bun_path_finds_fetch_script_output_when_present() {
        // This test passes when `scripts/fetch-bun.sh --target <host>` has
        // been run for the host. CI runs that as a build prereq; locally
        // the test is informational — assertion guarded behind the file
        // check so a stock checkout doesn't fail the suite.
        if let Some(p) = resolve_dev_bun_path() {
            assert!(
                p.is_file(),
                "resolve_dev_bun_path returned a non-file path: {}",
                p.display()
            );
            assert_eq!(p.file_name().and_then(|s| s.to_str()), Some(BUN_BIN_NAME));
        }
    }

    #[test]
    fn resolve_command_passes_through_non_bun_declarations() {
        // Absolute paths, ./bin shell-outs, and any other binary name must
        // round-trip unchanged so existing manifests (engine adapters, the
        // legacy iyke-mcp `./bin/iyke-mcp`) keep working.
        assert_eq!(
            resolve_command("./bin/iyke-mcp"),
            PathBuf::from("./bin/iyke-mcp")
        );
        assert_eq!(
            resolve_command("/usr/bin/node"),
            PathBuf::from("/usr/bin/node")
        );
        assert_eq!(resolve_command("uvx"), PathBuf::from("uvx"));
    }

    #[test]
    fn augmented_path_is_superset_of_inherited_path() {
        // The inherited $PATH must always be preserved (and come first) so a
        // user's explicit ordering still wins; we only append fallbacks.
        let built = build_augmented_path();
        let built_set: std::collections::HashSet<PathBuf> = std::env::split_paths(&built).collect();
        if let Some(inherited) = std::env::var_os("PATH") {
            for entry in std::env::split_paths(&inherited) {
                assert!(
                    built_set.contains(&entry),
                    "augmented PATH dropped inherited entry {}",
                    entry.display()
                );
            }
        }
        assert!(!built.is_empty(), "augmented PATH should never be empty");
    }

    #[test]
    fn resolve_command_bun_falls_through_to_path_when_uninitialised() {
        // Without `init_from_app`, BUN_PATH is unset → `"bun"` round-trips
        // and the OS does PATH lookup. This is the fallback contract for
        // debug builds where fetch-bun.sh hasn't been run.
        // (Cannot assert the bundled-path branch here without an AppHandle;
        // covered by the dev-path test above when the binary is present.)
        if bun_path().is_none() && fetched_runtime().is_none() {
            assert_eq!(resolve_command("bun"), PathBuf::from("bun"));
        }
    }

    #[test]
    fn is_bun_command_only_matches_bare_bun() {
        assert!(is_bun_command("bun"));
        assert!(!is_bun_command("./bin/iyke-mcp"));
        assert!(!is_bun_command("/usr/bin/node"));
        assert!(!is_bun_command("bun.exe"));
        assert!(!is_bun_command("uvx"));
    }

    #[test]
    fn version_ge_compares_semver_tuples() {
        // Pin is 1.3.14; accept equal-or-newer, reject older + unparsable.
        assert!(version_ge("1.3.14", "1.3.14")); // equal → accept
        assert!(version_ge("1.4.0", "1.3.14")); // newer minor
        assert!(version_ge("2.0.0", "1.3.14")); // newer major
        assert!(version_ge("1.3.15", "1.3.14")); // newer patch
        assert!(!version_ge("1.3.13", "1.3.14")); // older patch
        assert!(!version_ge("1.2.99", "1.3.14")); // older minor
        assert!(!version_ge("garbage", "1.3.14")); // unparsable → reject
        // Tolerate a leading `v` and pre-release suffix on the found string.
        assert!(version_ge("v1.3.14", "1.3.14"));
        assert!(version_ge("1.4.0-canary.1", "1.3.14"));
    }

    /// Drift guard: the Rust pin SoT (`BUN_VERSION` + `bun_zip_sha256`) must
    /// match `scripts/fetch-bun.sh`'s `BUN_VERSION` + `expected_sha_for()`
    /// case block for every supported target. A bun bump that updates one but
    /// not the other fails here loudly.
    #[test]
    fn pin_table_matches_fetch_bun_script() {
        let script = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../scripts/fetch-bun.sh"
        ))
        .expect("read fetch-bun.sh");

        // BUN_VERSION="x.y.z"
        let script_version = script
            .lines()
            .find_map(|l| {
                let l = l.trim();
                l.strip_prefix("BUN_VERSION=")
                    .map(|v| v.trim().trim_matches('"').to_string())
            })
            .expect("BUN_VERSION line in fetch-bun.sh");
        assert_eq!(
            script_version, BUN_VERSION,
            "BUN_VERSION drift: script={script_version} rust={BUN_VERSION}"
        );

        // Parse `    <target>)  echo "<sha>" ;;` arms.
        for target in [
            "linux-x64",
            "linux-aarch64",
            "darwin-x64",
            "darwin-aarch64",
            "windows-x64",
        ] {
            let arm = script
                .lines()
                .find(|l| l.trim_start().starts_with(&format!("{target})")))
                .unwrap_or_else(|| panic!("no case arm for {target} in fetch-bun.sh"));
            // Extract the sha between the quotes after `echo`.
            let sha = arm
                .split("echo")
                .nth(1)
                .and_then(|rest| rest.split('"').nth(1))
                .unwrap_or_else(|| panic!("could not parse sha for {target}: {arm}"));
            assert_eq!(
                Some(sha),
                bun_zip_sha256(target),
                "sha drift for {target}: script={sha} rust={:?}",
                bun_zip_sha256(target)
            );
        }
    }

    #[test]
    fn bun_zip_sha256_unknown_target_is_none() {
        assert!(bun_zip_sha256("plan9-risc").is_none());
    }
}
