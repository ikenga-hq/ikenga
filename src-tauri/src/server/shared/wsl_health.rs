//! WSL network health (WP-2): can a WSL session reach the network, and if
//! not, why — named precisely enough to pick the right fix.
//!
//! On 2026-10-07 a WSL-launched Claude session could not sign in
//! (`getaddrinfo EAI_AGAIN`) because WSL's mirrored networking had failed on
//! every boot for days (`CreateInstance/CreateVm/ConfigureNetworking/
//! 0x8007054f` → "falling back to networkingMode None"), leaving the distro
//! with loopback only. Windows itself was online. The in-distro symptoms
//! (no route, a dangling `/etc/resolv.conf`) don't name that cause; the WSL
//! entry in the Windows Application event log does, so the probe reads both.
//!
//! One probe is:
//!
//! 1. one `wsl.exe [-d <distro>] -e sh -c <fixed script>` that reports the
//!    default route, the non-loopback interfaces, `/etc/resolv.conf`'s state,
//!    whether `getent hosts` resolves [`HEALTH_HOST`], and uptime (to date
//!    the current WSL boot);
//! 2. a host-side resolution of the same host, to tell "Windows is offline"
//!    apart from "WSL is";
//! 3. a fixed PowerShell `Get-WinEvent` read of the Application log's `WSL`
//!    entries (their **properties** — `Message` is empty because the `WSL`
//!    provider has no registered manifest on most machines);
//! 4. `networkingMode` from `%USERPROFILE%\.wslconfig`.
//!
//! Results are cached [`CACHE_TTL`] per distro (D-7: probe before each WSL
//! launch and on a network errno, never in the background).
//!
//! The fixes ([`WslFixAction`]) run from least to most disruptive:
//! rewrite `/etc/resolv.conf` (`dns_only`), restart HNS + WSL through one UAC
//! prompt (`no_route` after a failed mirrored setup — a bare `wsl --shutdown`
//! does not fix that), switch `.wslconfig` to `networkingMode=nat` (D-6).
//!
//! Every spawn is async, bounded and windowless; no caller-supplied string is
//! ever interpolated into a shell or PowerShell script. Distro names are
//! checked against [`valid_distro_name`] (and, for fixes, the installed list)
//! and only ever passed as a separate `wsl.exe -d` argument.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The host both sides resolve. Windows' own connectivity-check host, so it
/// is always meant to resolve on a working network.
pub const HEALTH_HOST: &str = "www.msftconnecttest.com";

/// How long a probe result is reused for the same distro (D-7).
pub const CACHE_TTL: Duration = Duration::from_secs(30);

/// How far back the event log is read when the current WSL boot can't be
/// dated (the distro probe failed).
pub const EVENT_LOOKBACK: Duration = Duration::from_secs(24 * 60 * 60);

/// The mirrored-setup failure is logged while the VM is created, a few
/// seconds before the distro's clock starts. Events this long before the
/// computed boot time still count as "this boot".
const BOOT_SLACK_MS: i64 = 120_000;

/// The "An internal error occurred … ConfigureNetworking/0x…" entry is
/// written next to the "falling back" one (sub-millisecond apart on the
/// machine that surfaced this). Pair them when this close.
const PAIRING_WINDOW_MS: i64 = 30_000;

/// The address WSL's DNS tunnelling proxy listens on inside the distro.
pub const DNS_TUNNEL_ADDR: &str = "10.255.255.254";

/// Last-resort public resolver for [`WslFixAction::RepairDns`].
const FALLBACK_NAMESERVER: &str = "1.1.1.1";

// ─── Types ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WslHealthState {
    /// The distro resolves names.
    Ok,
    /// Windows can't resolve names either: the problem isn't WSL's.
    HostOffline,
    /// The distro has no default route (typically loopback only).
    NoRoute,
    /// The distro has a route but can't resolve names.
    DnsOnly,
    /// `wsl.exe` failed, timed out or named a WSL error.
    WslDown,
    /// No `wsl.exe`, or no installed distribution.
    NotInstalled,
}

impl WslHealthState {
    pub fn as_str(self) -> &'static str {
        match self {
            WslHealthState::Ok => "ok",
            WslHealthState::HostOffline => "host_offline",
            WslHealthState::NoRoute => "no_route",
            WslHealthState::DnsOnly => "dns_only",
            WslHealthState::WslDown => "wsl_down",
            WslHealthState::NotInstalled => "not_installed",
        }
    }

    /// States whose cause is WSL itself — what the `fix.wsl_network`
    /// notification is raised for. `host_offline` is Windows' problem and
    /// `not_installed` is not a failure.
    pub fn is_wsl_fault(self) -> bool {
        matches!(
            self,
            WslHealthState::NoRoute | WslHealthState::DnsOnly | WslHealthState::WslDown
        )
    }
}

/// The newest "falling back to networkingMode None" WSL event in scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirroredFailure {
    /// Unix ms the event was logged.
    pub at: i64,
    /// The HRESULT of the paired `ConfigureNetworking` error, e.g.
    /// `0x8007054f`, when one was logged next to it.
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WslHealth {
    pub state: WslHealthState,
    /// The distro probed; `None` = the default distro.
    pub distro: Option<String>,
    /// One short human sentence naming the cause.
    pub detail: String,
    pub mirrored_failure: Option<MirroredFailure>,
    /// `[wsl2] networkingMode` from `.wslconfig`, lower-cased; `None` when
    /// unset (WSL's default is NAT) or unreadable.
    pub networking_mode: Option<String>,
    /// Unix ms the probe ran.
    pub checked_at: i64,
}

/// A fix the UI can ask for. Wire form: `"repair_dns"` etc.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WslFixAction {
    RepairDns,
    RestartNetworking,
    SwitchToNat,
}

/// What a fix did. Wire form: `{"outcome":"done","health":{…}}`,
/// `{"outcome":"cancelled_by_user"}`, `{"outcome":"failed","reason":"…"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WslFixOutcome {
    /// The fix ran; `health` is a fresh (forced) re-probe.
    Done {
        health: WslHealth,
    },
    /// The user declined the UAC prompt.
    CancelledByUser,
    Failed {
        reason: String,
    },
}

/// `true` for a name WSL could register and that is safe as a lone argv
/// element: 1–64 of `[A-Za-z0-9._-]`, not starting with `-`.
pub fn valid_distro_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ─── Distro probe: script + parse ───────────────────────────────────────────

/// The fixed in-distro probe. POSIX `sh`, no iproute2 needed (reads
/// `/proc/net/route` and `/sys/class/net`), one `key=value` per line.
/// The lookup uses `getent hosts`, else `nslookup` (busybox / Alpine), and
/// reports `dns=unknown` when the distro has neither — a missing tool is not
/// a DNS failure. It is bounded with `timeout` when the distro has it, and a
/// lookup cut off by that cap reports `dns=timeout` (exit 124) apart from
/// one that failed outright.
pub const DISTRO_PROBE_SCRIPT: &str = concat!(
    "echo ikenga_probe=1\n",
    "r=0\n",
    "if [ -r /proc/net/route ]; then while read -r i d rest; do [ \"$d\" = 00000000 ] && r=1; done < /proc/net/route; fi\n",
    "echo route=$r\n",
    "printf 'ifaces='; for n in /sys/class/net/*; do b=${n##*/}; [ \"$b\" = lo ] || printf '%s ' \"$b\"; done; echo\n",
    "if [ -L /etc/resolv.conf ] && [ ! -e /etc/resolv.conf ]; then echo resolv=dangling\n",
    "elif [ ! -e /etc/resolv.conf ]; then echo resolv=missing\n",
    "elif [ ! -r /etc/resolv.conf ]; then echo resolv=unreadable\n",
    "else echo \"resolv=ok $(grep -c '^[[:space:]]*nameserver' /etc/resolv.conf)\"; fi\n",
    "t=; command -v timeout >/dev/null 2>&1 && t='timeout 8'\n",
    "q=; if command -v getent >/dev/null 2>&1; then q='getent hosts'; elif command -v nslookup >/dev/null 2>&1; then q=nslookup; fi\n",
    "if [ -z \"$q\" ]; then echo dns=unknown\n",
    "else $t $q www.msftconnecttest.com >/dev/null 2>&1; c=$?\n",
    "if [ $c -eq 0 ]; then echo dns=1; elif [ -n \"$t\" ] && [ $c -eq 124 ]; then echo dns=timeout; else echo dns=0; fi; fi\n",
    "tun=0; grep -q '10\\.255\\.255\\.254' /proc/net/fib_trie 2>/dev/null && tun=1; echo dnstunnel=$tun\n",
    "echo \"uptime=$(cut -d' ' -f1 /proc/uptime 2>/dev/null)\"\n",
);

/// What `/etc/resolv.conf` looked like.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvState {
    /// Readable, with this many `nameserver` lines.
    Ok(u32),
    /// A symlink to nothing (WSL's `/mnt/wsl/resolv.conf` was never written).
    Dangling,
    Missing,
    Unreadable,
}

/// How the in-distro lookup of [`HEALTH_HOST`] went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DnsCheck {
    Resolved,
    /// The lookup failed (no answer, NXDOMAIN, no nameserver …).
    Failed,
    /// The lookup ran past the script's 8 s cap. glibc's own default (5 s ×
    /// 2 attempts) gives up later than that, so a nameserver that never
    /// answers lands here — it is a failure, named apart in the detail.
    TimedOut,
    /// The distro has no `getent` or `nslookup`: resolution wasn't tested.
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DistroProbe {
    pub has_default_route: bool,
    /// Interfaces other than `lo`.
    pub interfaces: Vec<String>,
    pub resolv: ResolvState,
    /// The lookup of [`HEALTH_HOST`].
    pub dns: DnsCheck,
    /// The DNS tunnelling address is configured (mirrored / dnsTunneling).
    pub dns_tunnel: bool,
    pub uptime_secs: Option<f64>,
}

/// Parse [`DISTRO_PROBE_SCRIPT`]'s output. `None` unless the marker line is
/// present — anything else is WSL's own output, not the script's.
pub fn parse_distro_probe(stdout: &str) -> Option<DistroProbe> {
    let mut marker = false;
    let mut probe = DistroProbe {
        has_default_route: false,
        interfaces: Vec::new(),
        resolv: ResolvState::Unreadable,
        dns: DnsCheck::Failed,
        dns_tunnel: false,
        uptime_secs: None,
    };
    for line in stdout.lines() {
        let line = line.trim_matches(|c: char| c == '\0' || c.is_whitespace());
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key {
            "ikenga_probe" => marker = true,
            "route" => probe.has_default_route = value == "1",
            "ifaces" => {
                probe.interfaces = value.split_whitespace().map(str::to_string).collect();
            }
            "resolv" => {
                probe.resolv = match value.split_once(' ') {
                    Some(("ok", n)) => ResolvState::Ok(n.trim().parse().unwrap_or(0)),
                    None if value == "ok" => ResolvState::Ok(0),
                    _ => match value {
                        "dangling" => ResolvState::Dangling,
                        "missing" => ResolvState::Missing,
                        _ => ResolvState::Unreadable,
                    },
                }
            }
            "dns" => {
                probe.dns = match value {
                    "1" => DnsCheck::Resolved,
                    "timeout" => DnsCheck::TimedOut,
                    "unknown" => DnsCheck::Unknown,
                    _ => DnsCheck::Failed,
                }
            }
            "dnstunnel" => probe.dns_tunnel = value == "1",
            "uptime" => probe.uptime_secs = value.parse().ok(),
            _ => {}
        }
    }
    marker.then_some(probe)
}

/// Why the distro probe produced no [`DistroProbe`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeFailure {
    /// No `wsl.exe`, or WSL says it has no distribution.
    NotInstalled(String),
    /// `wsl.exe` failed or answered with something else.
    Down(String),
    /// `wsl.exe` didn't answer within the budget. Kept apart from [`Down`]
    /// so the probe can retry once: a first launch after a Windows boot can
    /// be that slow without anything being wrong.
    ///
    /// [`Down`]: ProbeFailure::Down
    TimedOut(String),
}

/// Read a finished `wsl.exe … sh -c <probe>`: the parsed probe, or why not.
pub fn read_distro_probe(
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<DistroProbe, ProbeFailure> {
    let out = super::wsl::decode_wsl_output(stdout);
    if let Some(p) = parse_distro_probe(&out) {
        return Ok(p);
    }
    let err = one_line(&format!(
        "{} {}",
        super::wsl::decode_wsl_output(stderr),
        out
    ));
    let lower = err.to_ascii_lowercase();
    if lower.contains("no installed distributions") {
        return Err(ProbeFailure::NotInstalled(
            "WSL has no installed distribution".into(),
        ));
    }
    if lower.contains("no distribution with the supplied name")
        || lower.contains("wsl_e_distro_not_found")
    {
        return Err(ProbeFailure::NotInstalled(
            "That WSL distribution isn't installed".into(),
        ));
    }
    Err(ProbeFailure::Down(if err.is_empty() {
        format!(
            "wsl.exe exited {} without running the probe",
            code.map_or("abnormally".to_string(), |c| format!("with code {c}"))
        )
    } else {
        err.chars().take(240).collect()
    }))
}

/// Collapse whitespace (wsl.exe errors span lines) and trim.
fn one_line(s: &str) -> String {
    s.replace('\0', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// ─── Event log: script + parse ──────────────────────────────────────────────

/// The fixed event-log read: the Application log's `WSL` entries from the
/// last 24 h, newest first, one `<unix ms>\t<properties joined by " | ">`
/// per line. XPath on the provider name works even though `WSL` has no
/// registered provider manifest (where `-FilterHashtable @{ProviderName}`
/// throws `NoMatchingProvidersFound`). No events is exit 0, not an error.
pub const EVENT_LOG_SCRIPT: &str = concat!(
    "$ErrorActionPreference = 'Stop'\n",
    "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8\n",
    "try {\n",
    "  $events = Get-WinEvent -LogName Application -MaxEvents 200 -FilterXPath \"*[System[Provider[@Name='WSL'] and TimeCreated[timediff(@SystemTime) <= 86400000]]]\"\n",
    "} catch {\n",
    "  if ($_.FullyQualifiedErrorId -like 'NoMatchingEventsFound*') { exit 0 }\n",
    "  [Console]::Error.WriteLine($_.Exception.Message); exit 2\n",
    "}\n",
    "foreach ($e in $events) {\n",
    "  $props = ($e.Properties | ForEach-Object { [string]$_.Value }) -join ' | '\n",
    "  $ms = ([DateTimeOffset]$e.TimeCreated).ToUnixTimeMilliseconds()\n",
    "  \"{0}`t{1}\" -f $ms, ($props -replace '\\s+', ' ')\n",
    "}\n",
);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WslEvent {
    /// Unix ms.
    pub at: i64,
    /// The event's properties, joined.
    pub text: String,
}

/// Parse [`EVENT_LOG_SCRIPT`]'s output. Malformed lines are skipped.
pub fn parse_event_lines(stdout: &str) -> Vec<WslEvent> {
    stdout
        .lines()
        .filter_map(|line| {
            let (ms, text) = line.trim_end_matches('\r').split_once('\t')?;
            Some(WslEvent {
                at: ms.trim().trim_start_matches('\u{feff}').parse().ok()?,
                text: text.trim().to_string(),
            })
        })
        .collect()
}

fn is_mirrored_fallback(text: &str) -> bool {
    text.to_ascii_lowercase()
        .contains("falling back to networkingmode none")
}

/// The HRESULT in a `…/ConfigureNetworking/0x8007054f`-style error.
fn configure_networking_code(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let at = lower.find("configurenetworking")?;
    let rest = &lower[at..];
    let hex = rest.find("0x")?;
    let code: String = rest[hex + 2..]
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .collect();
    (code.len() == 8).then(|| format!("0x{code}"))
}

/// The newest mirrored-networking fallback at or after `since_ms`, paired
/// with the `ConfigureNetworking` error logged next to it.
pub fn newest_mirrored_failure(events: &[WslEvent], since_ms: i64) -> Option<MirroredFailure> {
    let fallback = events
        .iter()
        .filter(|e| e.at >= since_ms && is_mirrored_fallback(&e.text))
        .max_by_key(|e| e.at)?;
    let error_code = configure_networking_code(&fallback.text).or_else(|| {
        events
            .iter()
            .filter(|e| (e.at - fallback.at).abs() <= PAIRING_WINDOW_MS)
            .filter_map(|e| {
                configure_networking_code(&e.text).map(|c| ((e.at - fallback.at).abs(), c))
            })
            .min_by_key(|(distance, _)| *distance)
            .map(|(_, c)| c)
    });
    Some(MirroredFailure {
        at: fallback.at,
        error_code,
    })
}

/// The earliest event time that belongs to "this WSL boot": `uptime` before
/// `checked_at` (less [`BOOT_SLACK_MS`]), never more than
/// [`EVENT_LOOKBACK`] ago.
pub fn event_window_start(checked_at: i64, uptime_secs: Option<f64>) -> i64 {
    let lookback = checked_at - EVENT_LOOKBACK.as_millis() as i64;
    match uptime_secs {
        Some(up) if up.is_finite() && up >= 0.0 => {
            (checked_at - (up * 1000.0) as i64 - BOOT_SLACK_MS).max(lookback)
        }
        _ => lookback,
    }
}

// ─── .wslconfig ─────────────────────────────────────────────────────────────

fn section_name(line: &str) -> Option<String> {
    let t = line.trim();
    (t.starts_with('[') && t.ends_with(']') && t.len() >= 2)
        .then(|| t[1..t.len() - 1].trim().to_ascii_lowercase())
}

/// `Some(value)` when `line` is an active `networkingMode=` assignment.
fn networking_mode_value(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if t.starts_with('#') || t.starts_with(';') {
        return None;
    }
    let (key, value) = t.split_once('=')?;
    key.trim()
        .eq_ignore_ascii_case("networkingMode")
        .then(|| value.split(['#', ';']).next().unwrap_or("").trim())
}

/// `[wsl2] networkingMode` in a `.wslconfig`, lower-cased. The last
/// assignment wins, as in WSL's own parser.
pub fn networking_mode(wslconfig: &str) -> Option<String> {
    let mut section = None::<String>;
    let mut mode = None;
    // `trim()` keeps U+FEFF, so a BOM would hide a first-line `[wsl2]`.
    let wslconfig = wslconfig.strip_prefix('\u{feff}').unwrap_or(wslconfig);
    for line in wslconfig.lines() {
        if let Some(name) = section_name(line) {
            section = Some(name);
            continue;
        }
        if section.as_deref() == Some("wsl2") {
            if let Some(v) = networking_mode_value(line) {
                mode = (!v.is_empty()).then(|| v.to_ascii_lowercase());
            }
        }
    }
    mode
}

/// `wslconfig` with `[wsl2] networkingMode=nat`, every other line and
/// comment untouched. Each active `networkingMode` line under `[wsl2]` is
/// replaced in place (so a duplicate can't win with the old value); with
/// none, the line goes right after the `[wsl2]` header; with no `[wsl2]`
/// section, one is appended. Line endings (CRLF / LF), a BOM and the
/// trailing newline are kept.
pub fn set_networking_mode_nat(wslconfig: &str) -> String {
    const LINE: &str = "networkingMode=nat";
    let (bom, body) = match wslconfig.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", wslconfig),
    };
    let eol = if body.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing_newline = body.is_empty() || body.ends_with('\n');
    let mut lines: Vec<String> = body.lines().map(str::to_string).collect();

    let mut section = None::<String>;
    let mut header_at = None;
    let mut replaced = false;
    for (i, line) in lines.iter_mut().enumerate() {
        if let Some(name) = section_name(line) {
            if name == "wsl2" && header_at.is_none() {
                header_at = Some(i);
            }
            section = Some(name);
            continue;
        }
        if section.as_deref() == Some("wsl2") && networking_mode_value(line).is_some() {
            let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            *line = format!("{indent}{LINE}");
            replaced = true;
        }
    }
    if !replaced {
        match header_at {
            Some(i) => lines.insert(i + 1, LINE.to_string()),
            None => {
                if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                    lines.push(String::new());
                }
                lines.push("[wsl2]".to_string());
                lines.push(LINE.to_string());
            }
        }
    }
    let mut out = String::from(bom);
    out.push_str(&lines.join(eol));
    if trailing_newline || header_at.is_none() {
        out.push_str(eol);
    }
    out
}

/// How a `.wslconfig` is stored on disk, so an edit is written back the
/// same way. Notepad and PowerShell 5's `Out-File` both write UTF-16.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WslConfigEncoding {
    Utf8 { bom: bool },
    Utf16Le { bom: bool },
    Utf16Be { bom: bool },
}

/// Decode a `.wslconfig`: UTF-8 (BOM optional), or UTF-16 LE / BE with or
/// without a BOM. The returned text never starts with a BOM. `Err` for
/// anything else — notably a NUL byte that isn't UTF-16 framing: UTF-16
/// ASCII text is otherwise *valid UTF-8* (NULs in between), and an edit
/// would append UTF-8 lines to a UTF-16 file.
pub fn decode_wslconfig(bytes: &[u8]) -> Result<(String, WslConfigEncoding), String> {
    fn utf16(bytes: &[u8], le: bool) -> Result<String, String> {
        if bytes.len() % 2 != 0 {
            return Err("odd-length UTF-16".into());
        }
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| {
                if le {
                    u16::from_le_bytes([c[0], c[1]])
                } else {
                    u16::from_be_bytes([c[0], c[1]])
                }
            })
            .collect();
        String::from_utf16(&units).map_err(|_| "invalid UTF-16".to_string())
    }
    let unsupported =
        |why: &str| format!(".wslconfig isn't in a text encoding Ikenga can edit safely ({why})");
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return std::str::from_utf8(rest)
            .map(|t| (t.to_string(), WslConfigEncoding::Utf8 { bom: true }))
            .map_err(|_| unsupported("invalid UTF-8"));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, true)
            .map(|t| (t, WslConfigEncoding::Utf16Le { bom: true }))
            .map_err(|e| unsupported(&e));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, false)
            .map(|t| (t, WslConfigEncoding::Utf16Be { bom: true }))
            .map_err(|e| unsupported(&e));
    }
    if bytes.contains(&0) {
        // BOM-less UTF-16: an ASCII config has its NULs all on one side.
        let even_nuls = bytes.iter().step_by(2).filter(|b| **b == 0).count();
        let odd_nuls = bytes.iter().skip(1).step_by(2).filter(|b| **b == 0).count();
        let le = odd_nuls > 0 && even_nuls == 0;
        let be = even_nuls > 0 && odd_nuls == 0;
        if bytes.len() % 2 == 0 && (le || be) {
            if let Ok(t) = utf16(bytes, le) {
                if !t.contains('\0') {
                    let encoding = if le {
                        WslConfigEncoding::Utf16Le { bom: false }
                    } else {
                        WslConfigEncoding::Utf16Be { bom: false }
                    };
                    return Ok((t, encoding));
                }
            }
        }
        return Err(unsupported("it contains NUL bytes"));
    }
    std::str::from_utf8(bytes)
        .map(|t| (t.to_string(), WslConfigEncoding::Utf8 { bom: false }))
        .map_err(|_| unsupported("invalid UTF-8"))
}

/// `text` written back as [`decode_wslconfig`] found it (BOM included).
pub fn encode_wslconfig(text: &str, encoding: WslConfigEncoding) -> Vec<u8> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let (bom, mut out): (bool, Vec<u8>) = match encoding {
        WslConfigEncoding::Utf8 { bom } => (bom, text.as_bytes().to_vec()),
        WslConfigEncoding::Utf16Le { bom } => (
            bom,
            text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        ),
        WslConfigEncoding::Utf16Be { bom } => (
            bom,
            text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        ),
    };
    if bom {
        let mark: &[u8] = match encoding {
            WslConfigEncoding::Utf8 { .. } => &[0xEF, 0xBB, 0xBF],
            WslConfigEncoding::Utf16Le { .. } => &[0xFF, 0xFE],
            WslConfigEncoding::Utf16Be { .. } => &[0xFE, 0xFF],
        };
        out.splice(0..0, mark.iter().copied());
    }
    out
}

/// `%USERPROFILE%\.wslconfig`.
pub fn wslconfig_path() -> Option<std::path::PathBuf> {
    crate::platform::home_dir().map(|h| h.join(".wslconfig"))
}

#[cfg(windows)]
fn read_networking_mode() -> Option<String> {
    let bytes = std::fs::read(wslconfig_path()?).ok()?;
    networking_mode(&decode_wslconfig(&bytes).ok()?.0)
}

// ─── Classification ─────────────────────────────────────────────────────────

/// The verdict from the probe's three inputs. A negative is only issued on
/// evidence: the distro must have answered for `no_route` / `dns_only`, and
/// `host_offline` needs Windows' own lookup to have failed too — unless a
/// failed mirrored setup is logged for this boot, which names WSL as the
/// cause whatever the host is doing.
pub fn classify(
    probe: &Result<DistroProbe, ProbeFailure>,
    host_dns_ok: bool,
    mirrored: Option<&MirroredFailure>,
) -> (WslHealthState, String) {
    let mirrored_note = |base: &str| match mirrored {
        Some(m) => format!(
            "{base} WSL's mirrored networking failed to start{} and fell back to no network.",
            m.error_code
                .as_deref()
                .map(|c| format!(" ({c})"))
                .unwrap_or_default()
        ),
        None => base.to_string(),
    };
    let p = match probe {
        Err(ProbeFailure::NotInstalled(why)) => return (WslHealthState::NotInstalled, why.clone()),
        Err(ProbeFailure::Down(why) | ProbeFailure::TimedOut(why)) => {
            return (WslHealthState::WslDown, format!("WSL didn't answer: {why}"))
        }
        Ok(p) => p,
    };
    if p.dns == DnsCheck::Resolved {
        return (WslHealthState::Ok, "WSL can reach the network.".into());
    }
    if !p.has_default_route {
        if !host_dns_ok && mirrored.is_none() {
            return (
                WslHealthState::HostOffline,
                "Windows is offline too, so WSL has no network.".into(),
            );
        }
        let base = if p.interfaces.is_empty() {
            "WSL has no network: only the loopback interface is up."
        } else {
            "WSL has no network route."
        };
        return (WslHealthState::NoRoute, mirrored_note(base));
    }
    if !host_dns_ok {
        return (
            WslHealthState::HostOffline,
            "Windows can't resolve names either, so this isn't a WSL problem.".into(),
        );
    }
    // No lookup tool: only a broken resolv.conf is evidence of a DNS fault.
    if p.dns == DnsCheck::Unknown && matches!(p.resolv, ResolvState::Ok(n) if n > 0) {
        return (
            WslHealthState::Ok,
            "WSL has a network route (this distro has no getent or nslookup, so name lookups weren't tested).".into(),
        );
    }
    let why = match p.resolv {
        ResolvState::Dangling => {
            "WSL can't resolve names: /etc/resolv.conf points at a file WSL never wrote."
        }
        ResolvState::Missing => "WSL can't resolve names: /etc/resolv.conf is missing.",
        ResolvState::Unreadable => "WSL can't resolve names: /etc/resolv.conf is unreadable.",
        ResolvState::Ok(0) => "WSL can't resolve names: /etc/resolv.conf lists no nameserver.",
        ResolvState::Ok(_) if p.dns == DnsCheck::TimedOut => {
            "WSL has a route but its DNS servers didn't answer within 8 seconds."
        }
        ResolvState::Ok(_) => "WSL has a route but its DNS servers don't answer.",
    };
    (WslHealthState::DnsOnly, why.into())
}

/// Nameservers for [`WslFixAction::RepairDns`]: the DNS tunnelling proxy when
/// the distro has it, then up to two of the host's own IPv4 servers, then a
/// public fallback — at most three (glibc reads no more). Every entry is a
/// parsed IPv4 address, so it is safe as an argv element.
pub fn repair_nameservers(dns_tunnel: bool, host_servers: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if dns_tunnel {
        out.push(DNS_TUNNEL_ADDR.to_string());
    }
    for line in host_servers.lines() {
        let Ok(ip) = line.trim().parse::<std::net::Ipv4Addr>() else {
            continue;
        };
        if ip.is_loopback() || ip.is_unspecified() || ip.is_link_local() || ip.is_broadcast() {
            continue;
        }
        let ip = ip.to_string();
        if !out.contains(&ip) && out.len() < 3 {
            out.push(ip);
        }
    }
    if out.len() < 3 && !out.iter().any(|s| s == FALLBACK_NAMESERVER) {
        out.push(FALLBACK_NAMESERVER.to_string());
    }
    out.truncate(3);
    out
}

/// The in-distro DNS repair, run as root with the nameservers as `$@`.
/// Backs up a regular `/etc/resolv.conf` (or records a symlink's target),
/// then atomically writes a plain file. Exit 0 on success.
pub const REPAIR_DNS_SCRIPT: &str = concat!(
    "set -e\n",
    "ts=$(date +%Y%m%d-%H%M%S)\n",
    "f=/etc/resolv.conf\n",
    "if [ -L \"$f\" ]; then readlink \"$f\" > \"$f.ikenga-bak-$ts.symlink\"; rm -f \"$f\"\n",
    "elif [ -f \"$f\" ]; then cp -p \"$f\" \"$f.ikenga-bak-$ts\"; fi\n",
    "tmp=\"$f.ikenga-tmp.$$\"\n",
    "{ echo \"# Written by Ikenga ($ts) to repair WSL DNS. The previous file is backed up next to this one.\"; for ns in \"$@\"; do echo \"nameserver $ns\"; done; } > \"$tmp\"\n",
    "chmod 644 \"$tmp\"\n",
    "mv -f \"$tmp\" \"$f\"\n",
);

/// How a UAC-elevated child ended, from the wrapper's exit code. The
/// wrapper ([`ELEVATED_RESTART_SCRIPT`]) exits 1223 (`ERROR_CANCELLED`) when
/// the user declines the prompt.
pub fn read_elevated_exit(code: Option<i32>, stderr: &str) -> Result<(), WslFixOutcome> {
    match code {
        Some(0) => Ok(()),
        Some(1223) => Err(WslFixOutcome::CancelledByUser),
        _ if stderr.to_ascii_lowercase().contains("canceled by the user")
            || stderr
                .to_ascii_lowercase()
                .contains("cancelled by the user") =>
        {
            Err(WslFixOutcome::CancelledByUser)
        }
        Some(10) => Err(WslFixOutcome::Failed {
            reason: "wsl --shutdown failed in the elevated step".into(),
        }),
        Some(11) => Err(WslFixOutcome::Failed {
            reason: "restarting the Host Network Service (hns) failed".into(),
        }),
        other => {
            let msg = one_line(stderr);
            Err(WslFixOutcome::Failed {
                reason: if msg.is_empty() {
                    format!(
                        "the elevated restart exited {}",
                        other.map_or("abnormally".to_string(), |c| format!("with code {c}"))
                    )
                } else {
                    msg.chars().take(240).collect()
                },
            })
        }
    }
}

/// What the elevated PowerShell runs: stop WSL, then restart HNS (which
/// rebuilds the mirrored virtual network on the next WSL start).
pub const ELEVATED_INNER_SCRIPT: &str = concat!(
    "$ErrorActionPreference = 'Stop'\n",
    "& wsl.exe --shutdown\n",
    "if ($LASTEXITCODE -ne 0) { exit 10 }\n",
    "try { Restart-Service -Name hns -Force } catch { exit 11 }\n",
    "exit 0\n",
);

/// The non-elevated wrapper: one UAC prompt for [`ELEVATED_INNER_SCRIPT`]
/// (passed as `-EncodedCommand`, substituted for `@ENCODED@` from a fixed
/// script — never caller input), waiting for it, and turning a declined
/// prompt into exit 1223.
pub const ELEVATED_RESTART_SCRIPT: &str = concat!(
    "$ErrorActionPreference = 'Stop'\n",
    "try {\n",
    "  $p = Start-Process -FilePath powershell.exe -Verb RunAs -Wait -PassThru -WindowStyle Hidden -ArgumentList '-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-EncodedCommand','@ENCODED@'\n",
    "  exit $p.ExitCode\n",
    "} catch {\n",
    "  $ex = $_.Exception\n",
    "  while ($ex) { if ($ex -is [System.ComponentModel.Win32Exception] -and $ex.NativeErrorCode -eq 1223) { exit 1223 }; $ex = $ex.InnerException }\n",
    "  [Console]::Error.WriteLine($_.Exception.Message); exit 1\n",
    "}\n",
);

/// `script` as PowerShell's `-EncodedCommand` wants it: base64 of UTF-16LE.
pub fn encode_powershell(script: &str) -> String {
    use base64::Engine as _;
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(utf16)
}

/// [`ELEVATED_RESTART_SCRIPT`] with the inner script encoded in.
pub fn elevated_restart_script() -> String {
    ELEVATED_RESTART_SCRIPT.replace("@ENCODED@", &encode_powershell(ELEVATED_INNER_SCRIPT))
}

/// Host IPv4 DNS servers, one per line.
pub const HOST_DNS_SCRIPT: &str =
    "Get-DnsClientServerAddress -AddressFamily IPv4 | ForEach-Object { $_.ServerAddresses }";

// ─── Probe + fixes (Windows) ────────────────────────────────────────────────

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::process::Output;
    use std::sync::{Arc, LazyLock, Mutex};
    use std::time::{Duration, Instant};

    use tokio::time::timeout;

    use super::*;
    use crate::executor::{PipedOpts, SpawnSpec, StdioMode};
    use crate::server::shared::wsl;

    const OPTS: PipedOpts = PipedOpts {
        stdin: StdioMode::Null,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Piped,
        kill_on_drop: true,
        no_console_window: true,
        detached: false,
        new_process_group: false,
    };

    /// The probe script itself (getent's 8 s cap plus headroom), on top of a
    /// cold WSL start.
    const PROBE_BUDGET: Duration = Duration::from_secs(12);
    const POWERSHELL_BUDGET: Duration = Duration::from_secs(20);
    const HOST_DNS_BUDGET: Duration = Duration::from_secs(6);
    /// The UAC prompt waits on a human.
    const ELEVATED_BUDGET: Duration = Duration::from_secs(300);

    type Slot = Arc<tokio::sync::Mutex<Option<(Instant, WslHealth)>>>;
    static CACHE: LazyLock<Mutex<HashMap<String, Slot>>> = LazyLock::new(Default::default);

    /// One fix at a time, machine-wide: two surfaces (banner + notification)
    /// clicked together must not raise two UAC prompts or race two
    /// `wsl --shutdown`s, and a DNS rewrite mid-restart is meaningless.
    static FIX_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    enum RunError {
        Failed(String),
        TimedOut(String),
    }

    impl RunError {
        fn message(self) -> String {
            match self {
                RunError::Failed(m) | RunError::TimedOut(m) => m,
            }
        }
    }

    fn slot(distro: Option<&str>) -> Slot {
        let key = distro.unwrap_or("").to_ascii_lowercase();
        let mut map = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(key).or_default().clone()
    }

    async fn run_checked(spec: SpawnSpec, budget: Duration) -> Result<Output, RunError> {
        let program = spec.program.to_string_lossy().into_owned();
        let child = crate::executor::current()
            .spawn_piped(spec, OPTS)
            .map_err(|e| RunError::Failed(format!("couldn't start {program}: {e}")))?;
        match timeout(budget, child.wait_with_output()).await {
            Err(_) => Err(RunError::TimedOut(format!(
                "{program} did not answer within {}s",
                budget.as_secs()
            ))),
            Ok(Err(e)) => Err(RunError::Failed(format!("{program} failed: {e}"))),
            Ok(Ok(out)) => Ok(out),
        }
    }

    async fn run(spec: SpawnSpec, budget: Duration) -> Result<Output, String> {
        run_checked(spec, budget).await.map_err(RunError::message)
    }

    /// `powershell.exe` running `script` as `-EncodedCommand`, so nothing in
    /// it is re-parsed by command-line quoting.
    fn powershell(script: &str) -> SpawnSpec {
        let mut spec = SpawnSpec::new("powershell.exe");
        spec.args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
            &encode_powershell(script),
        ]);
        spec
    }

    fn wsl_sh(distro: Option<&str>, root: bool, script: &str, args: &[String]) -> SpawnSpec {
        let mut spec = SpawnSpec::new("wsl.exe");
        spec.args(wsl::distro_args(distro));
        if root {
            spec.args(["-u", "root"]);
        }
        spec.args(["-e", "sh", "-c", script, "sh"]);
        spec.args(args);
        spec
    }

    async fn distro_probe_once(distro: Option<&str>) -> Result<DistroProbe, ProbeFailure> {
        match run_checked(
            wsl_sh(distro, false, DISTRO_PROBE_SCRIPT, &[]),
            PROBE_BUDGET + wsl::COLD_START,
        )
        .await
        {
            Err(RunError::TimedOut(e)) => Err(ProbeFailure::TimedOut(e)),
            Err(RunError::Failed(e)) => Err(ProbeFailure::Down(e)),
            Ok(out) => read_distro_probe(out.status.code(), &out.stdout, &out.stderr),
        }
    }

    /// The distro probe, retried once after a timeout: the first WSL start
    /// after a Windows boot can outlast one budget, and the second attempt
    /// finds the VM up. Two timeouts in a row is a wedged WSL.
    async fn distro_probe(distro: Option<&str>) -> Result<DistroProbe, ProbeFailure> {
        if !wsl::wsl_exe_present() {
            return Err(ProbeFailure::NotInstalled(
                "wsl.exe is not installed".into(),
            ));
        }
        match distro_probe_once(distro).await {
            Err(ProbeFailure::TimedOut(first)) => {
                tracing::info!(target: "ikenga::wsl", "WSL probe timed out ({first}); retrying once");
                distro_probe_once(distro).await
            }
            other => other,
        }
    }

    async fn host_dns_ok() -> bool {
        match timeout(HOST_DNS_BUDGET, tokio::net::lookup_host((HEALTH_HOST, 80))).await {
            Ok(Ok(mut addrs)) => addrs.next().is_some(),
            _ => false,
        }
    }

    async fn wsl_events() -> Vec<WslEvent> {
        match run(powershell(EVENT_LOG_SCRIPT), POWERSHELL_BUDGET).await {
            Ok(out) if out.status.success() => {
                parse_event_lines(&String::from_utf8_lossy(&out.stdout))
            }
            Ok(out) => {
                tracing::warn!(
                    target: "ikenga::wsl",
                    "reading the WSL event log: exit {:?}: {}",
                    out.status.code(),
                    one_line(&String::from_utf8_lossy(&out.stderr))
                );
                Vec::new()
            }
            Err(e) => {
                tracing::warn!(target: "ikenga::wsl", "reading the WSL event log: {e}");
                Vec::new()
            }
        }
    }

    async fn probe_now(distro: Option<&str>) -> WslHealth {
        let (probe, host_ok, events) =
            tokio::join!(distro_probe(distro), host_dns_ok(), wsl_events());
        let checked_at = now_ms();
        let uptime = probe.as_ref().ok().and_then(|p| p.uptime_secs);
        let mirrored = newest_mirrored_failure(&events, event_window_start(checked_at, uptime));
        let (state, detail) = classify(&probe, host_ok, mirrored.as_ref());
        WslHealth {
            state,
            distro: distro.map(str::to_string),
            detail,
            mirrored_failure: mirrored,
            networking_mode: read_networking_mode(),
            checked_at,
        }
    }

    pub async fn probe(distro: Option<&str>, force: bool) -> (WslHealth, bool) {
        let slot = slot(distro);
        let mut guard = slot.lock().await;
        if !force {
            if let Some((at, health)) = guard.as_ref() {
                if at.elapsed() < CACHE_TTL {
                    return (health.clone(), false);
                }
            }
        }
        let health = probe_now(distro).await;
        *guard = Some((Instant::now(), health.clone()));
        (health, true)
    }

    /// `Err` when `wsl.exe -l -q` answers and `distro` isn't in it. A list
    /// that can't be read doesn't block the fix — a wedged WSL is what
    /// `restart_networking` is for, and the name is already
    /// [`valid_distro_name`]-checked.
    async fn check_installed(distro: Option<&str>) -> Result<(), String> {
        let Some(d) = distro else { return Ok(()) };
        let mut spec = SpawnSpec::new("wsl.exe");
        spec.args(["-l", "-q"]);
        match run(spec, wsl::COLD_START).await {
            Ok(out) if out.status.success() => {
                let list = wsl::parse_distro_list(&out.stdout);
                if list.iter().any(|x| x.eq_ignore_ascii_case(d)) {
                    Ok(())
                } else {
                    Err(format!("no installed WSL distribution is named {d}"))
                }
            }
            _ => Ok(()),
        }
    }

    async fn reprobe(distro: Option<&str>) -> WslFixOutcome {
        WslFixOutcome::Done {
            health: probe(distro, true).await.0,
        }
    }

    pub async fn fix(action: WslFixAction, distro: Option<&str>) -> WslFixOutcome {
        let Ok(_one_at_a_time) = FIX_LOCK.try_lock() else {
            return WslFixOutcome::Failed {
                reason: "another WSL fix is already running".into(),
            };
        };
        if let Err(reason) = check_installed(distro).await {
            return WslFixOutcome::Failed { reason };
        }
        match action {
            WslFixAction::RepairDns => repair_dns(distro).await,
            WslFixAction::RestartNetworking => restart_networking(distro).await,
            WslFixAction::SwitchToNat => switch_to_nat(distro).await,
        }
    }

    async fn repair_dns(distro: Option<&str>) -> WslFixOutcome {
        let tunnel = distro_probe(distro)
            .await
            .map(|p| p.dns_tunnel)
            .unwrap_or(false);
        let host = match run(powershell(HOST_DNS_SCRIPT), POWERSHELL_BUDGET).await {
            Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
            Err(e) => {
                tracing::warn!(target: "ikenga::wsl", "reading host DNS servers: {e}");
                String::new()
            }
        };
        let servers = repair_nameservers(tunnel, &host);
        match run(
            wsl_sh(distro, true, REPAIR_DNS_SCRIPT, &servers),
            PROBE_BUDGET + wsl::COLD_START,
        )
        .await
        {
            Err(reason) => WslFixOutcome::Failed { reason },
            Ok(out) if !out.status.success() => WslFixOutcome::Failed {
                reason: format!(
                    "rewriting /etc/resolv.conf failed: {}",
                    one_line(&wsl::decode_wsl_output(&out.stderr))
                        .chars()
                        .take(200)
                        .collect::<String>()
                ),
            },
            Ok(_) => reprobe(distro).await,
        }
    }

    async fn restart_networking(distro: Option<&str>) -> WslFixOutcome {
        let out = match run_checked(powershell(&elevated_restart_script()), ELEVATED_BUDGET).await {
            Ok(out) => out,
            // The prompt belongs to consent.exe, not the wrapper we just
            // killed: it can still be approved, and the restart then runs.
            Err(RunError::TimedOut(_)) => {
                return WslFixOutcome::Failed {
                    reason: format!(
                        "the administrator prompt wasn't answered within {} minutes. If it is \
                         still open and you approve it, WSL restarts then; check again afterwards",
                        ELEVATED_BUDGET.as_secs() / 60
                    ),
                }
            }
            Err(RunError::Failed(reason)) => return WslFixOutcome::Failed { reason },
        };
        if let Err(outcome) =
            read_elevated_exit(out.status.code(), &String::from_utf8_lossy(&out.stderr))
        {
            return outcome;
        }
        reprobe(distro).await
    }

    async fn switch_to_nat(distro: Option<&str>) -> WslFixOutcome {
        let Some(path) = wslconfig_path() else {
            return WslFixOutcome::Failed {
                reason: "couldn't find the user profile folder".into(),
            };
        };
        let current = match std::fs::read(&path) {
            Ok(bytes) => match decode_wslconfig(&bytes) {
                Ok(decoded) => Some(decoded),
                Err(why) => {
                    return WslFixOutcome::Failed {
                        reason: format!("{why}; set networkingMode=nat under [wsl2] by hand"),
                    }
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return WslFixOutcome::Failed {
                    reason: format!("reading .wslconfig: {e}"),
                }
            }
        };
        let (text, encoding) = current
            .clone()
            .unwrap_or((String::new(), WslConfigEncoding::Utf8 { bom: false }));
        let next = set_networking_mode_nat(&text);
        if current.as_ref().map(|(t, _)| t.as_str()) != Some(next.as_str()) {
            if current.is_some() {
                let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
                let backup = path.with_file_name(format!(".wslconfig.bak-{stamp}"));
                if let Err(e) = std::fs::copy(&path, &backup) {
                    return WslFixOutcome::Failed {
                        reason: format!("backing up .wslconfig: {e}"),
                    };
                }
            }
            let tmp = path.with_file_name(".wslconfig.ikenga-tmp");
            let bytes = encode_wslconfig(&next, encoding);
            if let Err(e) = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &path)) {
                let _ = std::fs::remove_file(&tmp);
                return WslFixOutcome::Failed {
                    reason: format!("writing .wslconfig: {e}"),
                };
            }
        }
        let mut spec = SpawnSpec::new("wsl.exe");
        spec.arg("--shutdown");
        match run(spec, wsl::COLD_START + Duration::from_secs(15)).await {
            Err(reason) => return WslFixOutcome::Failed { reason },
            Ok(out) if !out.status.success() => {
                return WslFixOutcome::Failed {
                    reason: format!(
                        "networkingMode is now nat, but wsl --shutdown failed: {}",
                        one_line(&wsl::decode_wsl_output(&out.stderr))
                    ),
                }
            }
            Ok(_) => {}
        }
        reprobe(distro).await
    }
}

/// Probe `distro` (`None` = the default distro), reusing a result younger
/// than [`CACHE_TTL`] unless `force`. Returns the health and whether it was
/// freshly measured (only fresh results should raise / resolve the
/// notification). Concurrent callers for one distro share a single probe.
#[cfg(windows)]
pub async fn probe(distro: Option<&str>, force: bool) -> (WslHealth, bool) {
    imp::probe(distro, force).await
}

/// Run one fix, then re-probe (forced) on success.
#[cfg(windows)]
pub async fn fix(action: WslFixAction, distro: Option<&str>) -> WslFixOutcome {
    imp::fix(action, distro).await
}

#[cfg(not(windows))]
pub async fn probe(distro: Option<&str>, _force: bool) -> (WslHealth, bool) {
    (
        WslHealth {
            state: WslHealthState::NotInstalled,
            distro: distro.map(str::to_string),
            detail: "WSL is Windows-only".into(),
            mirrored_failure: None,
            networking_mode: None,
            checked_at: now_ms(),
        },
        false,
    )
}

#[cfg(not(windows))]
pub async fn fix(_action: WslFixAction, _distro: Option<&str>) -> WslFixOutcome {
    WslFixOutcome::Failed {
        reason: "WSL is Windows-only".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FALLBACK: &str =
        "Failed to configure network (networkingMode Mirrored), falling back to networkingMode None.";
    const INTERNAL: &str =
        "An internal error occurred. Error code: CreateInstance/CreateVm/ConfigureNetworking/0x8007054f";

    fn probe(route: bool, ifaces: &[&str], resolv: ResolvState, dns: bool) -> DistroProbe {
        DistroProbe {
            has_default_route: route,
            interfaces: ifaces.iter().map(|s| s.to_string()).collect(),
            resolv,
            dns: if dns {
                DnsCheck::Resolved
            } else {
                DnsCheck::Failed
            },
            dns_tunnel: false,
            uptime_secs: Some(60.0),
        }
    }

    #[test]
    fn event_properties_parse_and_pair_the_error_code() {
        // The machine that surfaced this: the error and the fallback are
        // logged 0.7 ms apart, newest first, with older boots below.
        let out = format!(
            "1791354862684\t{FALLBACK}\r\n1791354862683\t{INTERNAL}\r\n\
             1791354860242\tUnknown key 'wsl2.autoMemoryReclaim' in C:\\Users\\me\\.wslconfig:20\r\n\
             1791354120080\t{FALLBACK}\r\nnot a line\r\n"
        );
        let events = parse_event_lines(&out);
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].at, 1791354862684);

        let m = newest_mirrored_failure(&events, 0).unwrap();
        assert_eq!(m.at, 1791354862684);
        assert_eq!(m.error_code.as_deref(), Some("0x8007054f"));

        // An unpaired fallback carries no code.
        let only_old: Vec<_> = events
            .iter()
            .filter(|e| e.at < 1791354800000)
            .cloned()
            .collect();
        let m = newest_mirrored_failure(&only_old, 0).unwrap();
        assert_eq!(m.at, 1791354120080);
        assert_eq!(m.error_code, None);

        // Out of the window → none.
        assert_eq!(newest_mirrored_failure(&events, 1791354862685), None);
        assert_eq!(newest_mirrored_failure(&[], 0), None);
    }

    #[test]
    fn event_window_is_this_boot_capped_at_a_day() {
        let now = 10_000_000_000;
        assert_eq!(
            event_window_start(now, Some(60.0)),
            now - 60_000 - BOOT_SLACK_MS
        );
        let day = EVENT_LOOKBACK.as_millis() as i64;
        assert_eq!(event_window_start(now, None), now - day);
        assert_eq!(event_window_start(now, Some(1e9)), now - day);
    }

    #[test]
    fn probe_script_resolves_the_health_host() {
        assert!(DISTRO_PROBE_SCRIPT.contains(&format!("$q {HEALTH_HOST} ")));
        assert!(DISTRO_PROBE_SCRIPT.contains("q='getent hosts'"));
        assert!(DISTRO_PROBE_SCRIPT.contains("q=nslookup"));
        assert!(DISTRO_PROBE_SCRIPT.contains("echo dns=unknown"));
        assert!(DISTRO_PROBE_SCRIPT.contains("$c -eq 124 ]; then echo dns=timeout"));
    }

    #[test]
    fn distro_probe_output_parses() {
        let out =
            "ikenga_probe=1\nroute=0\nifaces=\nresolv=dangling\ndns=0\ndnstunnel=1\nuptime=4.21\n";
        let p = parse_distro_probe(out).unwrap();
        assert!(!p.has_default_route);
        assert!(p.interfaces.is_empty());
        assert_eq!(p.resolv, ResolvState::Dangling);
        assert_eq!(p.dns, DnsCheck::Failed);
        assert!(p.dns_tunnel);
        assert_eq!(p.uptime_secs, Some(4.21));

        let p = parse_distro_probe(
            "ikenga_probe=1\r\nroute=1\r\nifaces=eth0 loopback0 \r\nresolv=ok 2\r\ndns=1\r\n",
        )
        .unwrap();
        assert!(p.has_default_route);
        assert_eq!(p.dns, DnsCheck::Resolved);
        assert_eq!(p.interfaces, ["eth0", "loopback0"]);
        assert_eq!(p.resolv, ResolvState::Ok(2));

        let dns = |v: &str| {
            parse_distro_probe(&format!("ikenga_probe=1\ndns={v}\n"))
                .unwrap()
                .dns
        };
        assert_eq!(dns("timeout"), DnsCheck::TimedOut);
        assert_eq!(dns("unknown"), DnsCheck::Unknown);

        assert_eq!(parse_distro_probe("route=1\ndns=1\n"), None);
    }

    #[test]
    fn wsl_failures_read_as_down_or_not_installed() {
        let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(u16::to_le_bytes).collect() };
        assert_eq!(
            read_distro_probe(
                Some(1),
                &utf16("Windows Subsystem for Linux has no installed distributions.\r\n"),
                b""
            ),
            Err(ProbeFailure::NotInstalled(
                "WSL has no installed distribution".into()
            ))
        );
        match read_distro_probe(
            Some(-1),
            &utf16(
                "An internal error occurred.\r\nError code: Wsl/Service/CreateInstance/E_FAIL\r\n",
            ),
            b"",
        ) {
            Err(ProbeFailure::Down(why)) => {
                assert!(why.contains("Wsl/Service/CreateInstance/E_FAIL"), "{why}")
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            read_distro_probe(None, b"", b""),
            Err(ProbeFailure::Down(_))
        ));
        // A named distro that doesn't exist is not "WSL isn't starting".
        assert!(matches!(
            read_distro_probe(
                Some(-1),
                &utf16("There is no distribution with the supplied name.\r\nError code: Wsl/Service/WSL_E_DISTRO_NOT_FOUND\r\n"),
                b""
            ),
            Err(ProbeFailure::NotInstalled(_))
        ));
    }

    #[test]
    fn missing_lookup_tool_or_a_timeout_is_told_apart() {
        let with = |resolv: ResolvState, dns: DnsCheck| {
            let mut p = probe(true, &["eth0"], resolv, false);
            p.dns = dns;
            Ok(p)
        };
        // No getent / nslookup on a healthy distro: not a DNS fault.
        let (state, detail) = classify(&with(ResolvState::Ok(1), DnsCheck::Unknown), true, None);
        assert_eq!(state, WslHealthState::Ok);
        assert!(detail.contains("weren't tested"), "{detail}");
        // …but a dangling resolv.conf is evidence on its own.
        assert_eq!(
            classify(&with(ResolvState::Dangling, DnsCheck::Unknown), true, None).0,
            WslHealthState::DnsOnly
        );
        // No route stays no_route whatever the lookup tool.
        let mut lo = probe(false, &[], ResolvState::Ok(1), false);
        lo.dns = DnsCheck::Unknown;
        assert_eq!(classify(&Ok(lo), true, None).0, WslHealthState::NoRoute);
        // A lookup cut off at 8 s is a DNS failure, named as such.
        let (state, detail) = classify(&with(ResolvState::Ok(1), DnsCheck::TimedOut), true, None);
        assert_eq!(state, WslHealthState::DnsOnly);
        assert!(detail.contains("8 seconds"), "{detail}");
        // A wsl.exe timeout (after the retry) is wsl_down.
        let slow = Err(ProbeFailure::TimedOut(
            "wsl.exe did not answer within 27s".into(),
        ));
        assert_eq!(classify(&slow, true, None).0, WslHealthState::WslDown);
    }

    #[test]
    fn classification_from_probe_outputs() {
        let mirrored = MirroredFailure {
            at: 1,
            error_code: Some("0x8007054f".into()),
        };
        // lo-only, mirrored setup failed → no_route naming the code.
        let lo_only = Ok(probe(false, &[], ResolvState::Dangling, false));
        let (state, detail) = classify(&lo_only, true, Some(&mirrored));
        assert_eq!(state, WslHealthState::NoRoute);
        assert!(
            detail.contains("0x8007054f") && detail.contains("loopback"),
            "{detail}"
        );
        // …even when Windows is offline too: the log names WSL as the cause.
        assert_eq!(
            classify(&lo_only, false, Some(&mirrored)).0,
            WslHealthState::NoRoute
        );
        // lo-only with no logged failure and Windows offline → host_offline.
        assert_eq!(
            classify(&lo_only, false, None).0,
            WslHealthState::HostOffline
        );
        assert_eq!(classify(&lo_only, true, None).0, WslHealthState::NoRoute);

        // route ok + DNS fail → dns_only (host fine), host_offline (host not).
        let dns = Ok(probe(true, &["eth0"], ResolvState::Dangling, false));
        let (state, detail) = classify(&dns, true, None);
        assert_eq!(state, WslHealthState::DnsOnly);
        assert!(detail.contains("resolv.conf"), "{detail}");
        assert_eq!(classify(&dns, false, None).0, WslHealthState::HostOffline);

        // wsl.exe failure → wsl_down; missing → not_installed.
        let down = Err(ProbeFailure::Down(
            "wsl.exe did not answer within 27s".into(),
        ));
        let (state, detail) = classify(&down, true, None);
        assert_eq!(state, WslHealthState::WslDown);
        assert!(detail.contains("27s"));
        let none = Err(ProbeFailure::NotInstalled(
            "wsl.exe is not installed".into(),
        ));
        assert_eq!(classify(&none, true, None).0, WslHealthState::NotInstalled);

        // resolves → ok, whatever else.
        let ok = Ok(probe(true, &["eth0"], ResolvState::Ok(1), true));
        assert_eq!(classify(&ok, false, None).0, WslHealthState::Ok);
    }

    #[test]
    fn state_wire_names_and_fault_set() {
        assert_eq!(
            serde_json::to_value(WslHealthState::DnsOnly).unwrap(),
            "dns_only"
        );
        assert_eq!(WslHealthState::HostOffline.as_str(), "host_offline");
        assert!(WslHealthState::NoRoute.is_wsl_fault());
        assert!(!WslHealthState::HostOffline.is_wsl_fault());
        assert!(!WslHealthState::NotInstalled.is_wsl_fault());
        assert!(!WslHealthState::Ok.is_wsl_fault());
        assert_eq!(
            serde_json::to_value(WslFixOutcome::CancelledByUser).unwrap(),
            serde_json::json!({ "outcome": "cancelled_by_user" })
        );
        assert_eq!(
            serde_json::from_value::<WslFixAction>(serde_json::json!("switch_to_nat")).unwrap(),
            WslFixAction::SwitchToNat
        );
    }

    #[test]
    fn distro_names_are_strict() {
        assert!(valid_distro_name("Ubuntu"));
        assert!(valid_distro_name("Ubuntu-24.04"));
        assert!(valid_distro_name("my_distro.1"));
        assert!(!valid_distro_name(""));
        assert!(!valid_distro_name("-u"));
        assert!(!valid_distro_name("Ubuntu; rm -rf /"));
        assert!(!valid_distro_name("a b"));
        assert!(!valid_distro_name("$(x)"));
        assert!(!valid_distro_name(&"a".repeat(65)));
    }

    #[test]
    fn wslconfig_networking_mode_reads_wsl2_only() {
        let text = "[wsl2]\nmemory=4GB\n# networkingMode=nat\nnetworkingMode=Mirrored # share\n[experimental]\nnetworkingMode=nat\n";
        assert_eq!(networking_mode(text).as_deref(), Some("mirrored"));
        assert_eq!(networking_mode("[wsl2]\nmemory=1GB\n"), None);
        assert_eq!(networking_mode(""), None);
        // A UTF-8 BOM before the first-line header (Notepad) still reads.
        assert_eq!(
            networking_mode("\u{feff}[wsl2]\r\nnetworkingMode=mirrored\r\n").as_deref(),
            Some("mirrored")
        );
    }

    #[test]
    fn wslconfig_encodings_round_trip_and_unknown_ones_are_refused() {
        let text = "[wsl2]\r\nnetworkingMode=mirrored\r\n";
        let le: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let be: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
        let cases: Vec<(Vec<u8>, WslConfigEncoding)> = vec![
            (
                text.as_bytes().to_vec(),
                WslConfigEncoding::Utf8 { bom: false },
            ),
            (
                [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat(),
                WslConfigEncoding::Utf8 { bom: true },
            ),
            (
                [&[0xFF, 0xFE][..], &le].concat(),
                WslConfigEncoding::Utf16Le { bom: true },
            ),
            // BOM-less UTF-16LE is *valid UTF-8* byte-wise — the old trap.
            (le.clone(), WslConfigEncoding::Utf16Le { bom: false }),
            (
                [&[0xFE, 0xFF][..], &be].concat(),
                WslConfigEncoding::Utf16Be { bom: true },
            ),
            (be.clone(), WslConfigEncoding::Utf16Be { bom: false }),
        ];
        for (bytes, want) in cases {
            let (decoded, enc) = decode_wslconfig(&bytes).unwrap();
            assert_eq!(enc, want);
            assert_eq!(decoded, text, "{want:?}");
            assert_eq!(networking_mode(&decoded).as_deref(), Some("mirrored"));
            // Unchanged text writes back byte-for-byte; the edit keeps the
            // encoding.
            assert_eq!(encode_wslconfig(&decoded, enc), bytes, "{want:?}");
            let edited = encode_wslconfig(&set_networking_mode_nat(&decoded), enc);
            let (again, enc2) = decode_wslconfig(&edited).unwrap();
            assert_eq!(enc2, want);
            assert_eq!(again, "[wsl2]\r\nnetworkingMode=nat\r\n");
        }
        // Stray NULs / invalid UTF-8: refuse, never guess.
        assert!(decode_wslconfig(b"[wsl2]\0\0\nx=1\n").is_err());
        assert!(decode_wslconfig(&[b'[', 0xFF, b']']).is_err());
    }

    #[test]
    fn nat_edit_replaces_an_existing_line_in_place() {
        let before = "[wsl2]\r\n# Share the host stack.\r\nmemory=24GB\r\nnetworkingMode=mirrored\r\nhostAddressLoopback=true\r\n\r\n[experimental]\r\nsparseVhd=true\r\n";
        let after = set_networking_mode_nat(before);
        assert_eq!(
            after,
            "[wsl2]\r\n# Share the host stack.\r\nmemory=24GB\r\nnetworkingMode=nat\r\nhostAddressLoopback=true\r\n\r\n[experimental]\r\nsparseVhd=true\r\n"
        );
        assert_eq!(networking_mode(&after).as_deref(), Some("nat"));
        // Idempotent.
        assert_eq!(set_networking_mode_nat(&after), after);
    }

    #[test]
    fn nat_edit_inserts_a_missing_key_after_the_header() {
        let before = "# my config\n[WSL2]\nmemory=8GB\n";
        assert_eq!(
            set_networking_mode_nat(before),
            "# my config\n[WSL2]\nnetworkingMode=nat\nmemory=8GB\n"
        );
    }

    #[test]
    fn nat_edit_creates_a_missing_section() {
        assert_eq!(
            set_networking_mode_nat("[experimental]\nsparseVhd=true\n"),
            "[experimental]\nsparseVhd=true\n\n[wsl2]\nnetworkingMode=nat\n"
        );
        assert_eq!(
            set_networking_mode_nat("[experimental]\r\nsparseVhd=true"),
            "[experimental]\r\nsparseVhd=true\r\n\r\n[wsl2]\r\nnetworkingMode=nat\r\n"
        );
        assert_eq!(set_networking_mode_nat(""), "[wsl2]\nnetworkingMode=nat\n");
    }

    #[test]
    fn nat_edit_keeps_comments_bom_and_rewrites_duplicates() {
        let before = "\u{feff}[wsl2]\n; networkingMode=mirrored (old)\n  networkingMode = mirrored\nswap=0\nnetworkingMode=mirrored\n[other]\nnetworkingMode=mirrored\n";
        let after = set_networking_mode_nat(before);
        assert_eq!(
            after,
            "\u{feff}[wsl2]\n; networkingMode=mirrored (old)\n  networkingMode=nat\nswap=0\nnetworkingMode=nat\n[other]\nnetworkingMode=mirrored\n"
        );
    }

    #[test]
    fn repair_nameservers_prefers_tunnel_then_host_then_fallback() {
        assert_eq!(
            repair_nameservers(
                true,
                "192.168.1.1\r\n192.168.1.1\r\nfe80::1\r\n127.0.0.1\r\n8.8.4.4\r\n9.9.9.9\r\n"
            ),
            ["10.255.255.254", "192.168.1.1", "8.8.4.4"]
        );
        assert_eq!(repair_nameservers(false, ""), ["1.1.1.1"]);
        assert_eq!(
            repair_nameservers(false, "10.0.0.1\n; rm -rf /\n"),
            ["10.0.0.1", "1.1.1.1"]
        );
    }

    #[test]
    fn elevated_exit_codes_map_to_outcomes() {
        assert_eq!(read_elevated_exit(Some(0), ""), Ok(()));
        assert_eq!(
            read_elevated_exit(Some(1223), ""),
            Err(WslFixOutcome::CancelledByUser)
        );
        assert_eq!(
            read_elevated_exit(Some(1), "This command cannot be run due to the error: The operation was canceled by the user."),
            Err(WslFixOutcome::CancelledByUser)
        );
        assert!(matches!(
            read_elevated_exit(Some(11), ""),
            Err(WslFixOutcome::Failed { reason }) if reason.contains("hns")
        ));
        assert!(matches!(
            read_elevated_exit(Some(5), "Access is denied."),
            Err(WslFixOutcome::Failed { reason }) if reason == "Access is denied."
        ));
    }

    #[test]
    fn elevated_script_encodes_the_fixed_inner_script() {
        use base64::Engine as _;
        let script = elevated_restart_script();
        assert!(!script.contains("@ENCODED@"));
        let encoded = script
            .split("'-EncodedCommand','")
            .nth(1)
            .and_then(|s| s.split('\'').next())
            .unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), ELEVATED_INNER_SCRIPT);
    }

    /// Read-only probe of the real machine. Run by hand:
    /// `cargo test --lib wsl_health::tests::live_probe -- --ignored --nocapture`
    #[cfg(windows)]
    #[tokio::test]
    #[ignore]
    async fn live_probe() {
        let distro = crate::server::shared::wsl::configured_distro();
        let (health, fresh) = super::probe(distro.as_deref(), true).await;
        println!("{}", serde_json::to_string_pretty(&health).unwrap());
        assert!(fresh);
    }
}
