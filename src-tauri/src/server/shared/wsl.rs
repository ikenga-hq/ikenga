//! The pieces every WSL launch and probe share: which distro to use, how to
//! name it on a `wsl.exe` command line, how long a cold start may take, and
//! how to read `wsl.exe -l -q`.
//!
//! Agent detection (`agents.rs`), the headless Chi runtime (`chi_exec.rs`),
//! shell detection (`shell_detect.rs`) and seat install state all launch
//! `wsl.exe`. They used to disagree: terminals honoured
//! `engines.agentWslDistro` while everything else asked the default distro,
//! so a CLI installed only in the configured distro was "not found" by Chi.

use std::path::PathBuf;
use std::time::Duration;

/// What a cold `wsl.exe` start can cost before the command inside it even
/// runs: the utility VM boots, then the distro's init (5–15 s observed).
/// Every probe that goes through `wsl.exe` adds this to its own budget, so a
/// slow start reads as slow, not as "not installed" or "signed out".
pub(crate) const COLD_START: Duration = Duration::from_secs(15);

/// `budget` for a probe of `exec`, plus [`COLD_START`] when it runs through
/// WSL (a `wsl:<name>:<path>` detection path).
pub(crate) fn probe_budget(exec: &std::path::Path, budget: Duration) -> Duration {
    if exec.to_string_lossy().starts_with("wsl:") {
        budget + COLD_START
    } else {
        budget
    }
}

/// The distro named by `engines.agentWslDistro` in the personal settings
/// file, or `None` for "the default distro". The terminal launches agents
/// with `wsl.exe -d <this>` (`src/terminal/claude-wrap.ts`), so every other
/// WSL launch and probe uses it too. An unreadable settings file falls back
/// to the default distro with a warning — the same thing an unset value means.
pub(crate) fn configured_distro() -> Option<String> {
    let home = crate::platform::home_dir()?;
    let path = super::settings::scope::personal_path(&home);
    let document = match super::settings::scope::read_document(&path) {
        Ok(doc) => doc.unwrap_or_default(),
        Err(e) => {
            tracing::warn!(target: "ikenga::wsl", "reading engines.agentWslDistro: {e}");
            return None;
        }
    };
    normalise_distro(
        document
            .resolved()
            .get_field("engines.agentWslDistro")
            .and_then(serde_json::Value::as_str),
    )
}

/// `None` for an unset, blank or `"default"` value (the settings page stores
/// `"default"` when the only WSL profile is the unnamed default one).
pub(crate) fn normalise_distro(raw: Option<&str>) -> Option<String> {
    let name = raw?.trim();
    if name.is_empty() || name.eq_ignore_ascii_case("default") {
        None
    } else {
        Some(name.to_string())
    }
}

/// The `wsl.exe` arguments that select `distro` (`-d <name>`), or none for
/// the default distro.
pub(crate) fn distro_args(distro: Option<&str>) -> Vec<String> {
    match distro {
        Some(d) => vec!["-d".to_string(), d.to_string()],
        None => Vec::new(),
    }
}

/// Docker Desktop registers `docker-desktop` and `docker-desktop-data`. They
/// are not user environments: no login shell, no home, nothing a user would
/// open a terminal in or install an agent into.
pub(crate) fn is_docker_desktop(distro: &str) -> bool {
    distro.to_ascii_lowercase().starts_with("docker-desktop")
}

/// Whether `wsl.exe` exists on this machine at all.
#[cfg(windows)]
pub(crate) fn wsl_exe_present() -> bool {
    which::which("wsl.exe").is_ok() || std::path::Path::new(r"C:\Windows\System32\wsl.exe").exists()
}

/// `wsl.exe` output as text. It writes UTF-16LE (with or without a BOM) on
/// most Windows builds and UTF-8 on some; read as UTF-8 the former comes out
/// with a NUL between every character.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn decode_wsl_output(bytes: &[u8]) -> String {
    let utf16 = bytes.len() >= 2 && (bytes[1] == 0 || (bytes[0] == 0xff && bytes[1] == 0xfe));
    if utf16 {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
            .trim_start_matches('\u{feff}')
            .to_string()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// The user distros in `wsl.exe -l -q` output: de-duplicated, in order,
/// Docker Desktop's internal distros dropped.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn parse_distro_list(stdout: &[u8]) -> Vec<String> {
    let mut distros: Vec<String> = Vec::new();
    for line in decode_wsl_output(stdout).lines() {
        let clean = line.trim().trim_matches('\0').trim();
        if clean.is_empty() || is_docker_desktop(clean) {
            continue;
        }
        if !distros.iter().any(|d| d == clean) {
            distros.push(clean.to_string());
        }
    }
    distros
}

/// A Windows path into a WSL distro's share — `\\wsl.localhost\<distro>\…`
/// or `\\wsl$\<distro>\…` (either slash) — as the Linux path inside that
/// distro, plus the distro it names. `None` for any other path.
pub(crate) fn unc_to_linux(p: &str) -> Option<(String, String)> {
    let norm = p.replace('\\', "/");
    let lower = norm.to_ascii_lowercase();
    let rest = ["//wsl.localhost/", "//wsl$/"]
        .iter()
        .find_map(|prefix| lower.starts_with(prefix).then(|| &norm[prefix.len()..]))?;
    let (distro, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if distro.is_empty() {
        return None;
    }
    let path = path.trim_end_matches('/');
    let path = if path.is_empty() { "/" } else { path };
    Some((distro.to_string(), path.to_string()))
}

/// Who a `wsl.exe [-d <distro>]` launch runs as, read from the WSL
/// registrations under `HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss`
/// without starting WSL: the distro it lands in (`distro`, or the default
/// one when `None`) and that distro's default uid, if one is registered.
/// `None` when the registry doesn't say (no WSL, a distro that isn't
/// registered, an unreadable key).
#[cfg(windows)]
pub(crate) fn launch_identity(distro: Option<&str>) -> Option<(String, Option<u32>)> {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    };
    const LXSS: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

    let lxss = wide(LXSS);
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: valid NUL-terminated key name and out-pointer.
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, lxss.as_ptr(), 0, KEY_READ, &mut key) } != 0 {
        return None;
    }
    let wanted = match distro {
        Some(d) => Some(d.to_string()),
        None => reg_sz(key, "", "DefaultDistribution")
            .and_then(|guid| reg_sz(key, &guid, "DistributionName")),
    };
    let mut found = None;
    if let Some(wanted) = wanted {
        let mut index = 0u32;
        loop {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            // SAFETY: `name` holds `len` u16s; optional out-params are null.
            let rc = unsafe {
                RegEnumKeyExW(
                    key,
                    index,
                    name.as_mut_ptr(),
                    &mut len,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if rc != 0 {
                break;
            }
            index += 1;
            let sub = String::from_utf16_lossy(&name[..len as usize]);
            if reg_sz(key, &sub, "DistributionName")
                .is_some_and(|n| n.eq_ignore_ascii_case(&wanted))
            {
                found = Some((wanted.clone(), reg_dword(key, &sub, "DefaultUid")));
                break;
            }
        }
    }
    // SAFETY: `key` was opened above.
    unsafe { RegCloseKey(key) };
    found
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn reg_sz(
    key: windows_sys::Win32::System::Registry::HKEY,
    sub: &str,
    value: &str,
) -> Option<String> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_SZ};
    let (sub, value) = (wide(sub), wide(value));
    let mut buf = [0u16; 512];
    let mut bytes = std::mem::size_of_val(&buf) as u32;
    // SAFETY: `buf` holds `bytes` bytes; RRF_RT_REG_SZ NUL-terminates.
    let rc = unsafe {
        RegGetValueW(
            key,
            sub.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if rc != 0 {
        return None;
    }
    let units = &buf[..(bytes as usize / 2)];
    let text = String::from_utf16_lossy(units);
    let text = text.trim_end_matches('\0').trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(windows)]
fn reg_dword(
    key: windows_sys::Win32::System::Registry::HKEY,
    sub: &str,
    value: &str,
) -> Option<u32> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_DWORD};
    let (sub, value) = (wide(sub), wide(value));
    let mut out = 0u32;
    let mut bytes = std::mem::size_of::<u32>() as u32;
    // SAFETY: `out` is a u32 and `bytes` its size.
    let rc = unsafe {
        RegGetValueW(
            key,
            sub.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut out as *mut u32).cast(),
            &mut bytes,
        )
    };
    (rc == 0).then_some(out)
}

/// The home directory `/etc/passwd` gives `uid`, as a Linux path.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn home_for_uid(passwd: &str, uid: u32) -> Option<String> {
    passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 6 || fields[2].trim().parse::<u32>().ok()? != uid {
            return None;
        }
        let home = fields[5].trim();
        home.starts_with('/').then(|| home.to_string())
    })
}

/// The WSL share roots Windows exposes, newest name first.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn share_roots() -> [PathBuf; 2] {
    [PathBuf::from(r"\\wsl.localhost"), PathBuf::from(r"\\wsl$")]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_blank_distros_mean_the_default() {
        assert_eq!(normalise_distro(None), None);
        assert_eq!(normalise_distro(Some("")), None);
        assert_eq!(normalise_distro(Some("  ")), None);
        assert_eq!(normalise_distro(Some("default")), None);
        assert_eq!(normalise_distro(Some(" Ubuntu ")), Some("Ubuntu".into()));
        assert!(distro_args(None).is_empty());
        assert_eq!(distro_args(Some("Debian")), ["-d", "Debian"]);
    }

    #[test]
    fn wsl_probe_budgets_include_a_cold_start() {
        let base = Duration::from_secs(5);
        assert_eq!(
            probe_budget(std::path::Path::new("wsl:claude:/usr/bin/claude"), base),
            base + COLD_START
        );
        assert_eq!(
            probe_budget(std::path::Path::new("/usr/bin/claude"), base),
            base
        );
    }

    #[test]
    fn distro_list_reads_utf16_and_drops_docker_desktop() {
        let text = "Ubuntu\r\ndocker-desktop\r\ndocker-desktop-data\r\nDebian\r\nUbuntu\r\n";
        let mut utf16: Vec<u8> = vec![0xff, 0xfe];
        for u in text.encode_utf16() {
            utf16.extend(u.to_le_bytes());
        }
        assert_eq!(parse_distro_list(&utf16), ["Ubuntu", "Debian"]);
        assert_eq!(parse_distro_list(text.as_bytes()), ["Ubuntu", "Debian"]);
        assert!(parse_distro_list(b"").is_empty());
    }

    #[test]
    fn unc_share_paths_become_linux_paths() {
        assert_eq!(
            unc_to_linux(r"\\wsl.localhost\Ubuntu\home\me\proj"),
            Some(("Ubuntu".into(), "/home/me/proj".into()))
        );
        assert_eq!(
            unc_to_linux(r"\\wsl$\Debian\srv\"),
            Some(("Debian".into(), "/srv".into()))
        );
        assert_eq!(
            unc_to_linux("//WSL.LOCALHOST/Ubuntu"),
            Some(("Ubuntu".into(), "/".into()))
        );
        assert_eq!(unc_to_linux(r"\\server\share\x"), None);
        assert_eq!(unc_to_linux(r"C:\Users\me"), None);
        assert_eq!(unc_to_linux(r"\\wsl.localhost\"), None);
    }

    #[test]
    fn passwd_names_the_default_users_home() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\n\
                      daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin\n\
                      me:x:1000:1000:Me,,,:/home/me:/bin/bash\n\
                      odd:x:1001:1001::relative:/bin/sh\n";
        assert_eq!(home_for_uid(passwd, 0).as_deref(), Some("/root"));
        assert_eq!(home_for_uid(passwd, 1000).as_deref(), Some("/home/me"));
        assert_eq!(home_for_uid(passwd, 1001), None);
        assert_eq!(home_for_uid(passwd, 4242), None);
        assert_eq!(home_for_uid("", 0), None);
    }

    /// Reads the real registry; asserts only shape, since the machine may
    /// have no WSL at all.
    #[cfg(windows)]
    #[test]
    fn launch_identity_never_panics() {
        if let Some((name, _uid)) = launch_identity(None) {
            assert!(!name.is_empty());
            assert!(launch_identity(Some(&name)).is_some());
        }
        assert_eq!(launch_identity(Some("no-such-distro-ikenga-test")), None);
    }
}
