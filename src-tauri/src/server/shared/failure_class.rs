//! Classify a failed child process's output into an *infrastructure* cause,
//! so a probe that could not run is reported as "couldn't tell, because X"
//! instead of a confident negative ("not signed in", "not installed").
//!
//! Shared by agent detection (`agents.rs`) and the Chi run reader tasks
//! (`chi_exec.rs`). Callers append [`FailureClass::describe`] — a fixed
//! string plus the matched error token — never the raw output, which can
//! carry prompts and paths.

use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureClass {
    /// WSL itself failed: no distro, the VM wouldn't start, a `Wsl/…` error.
    WslUnavailable,
    /// The process ran but had no network (DNS or route). Carries the token
    /// that matched, e.g. `EAI_AGAIN`.
    Network(String),
}

impl FailureClass {
    pub fn describe(&self) -> String {
        match self {
            FailureClass::WslUnavailable => "WSL failed to start or has no distribution".into(),
            FailureClass::Network(token) => format!("network unreachable from the engine ({token})"),
        }
    }
}

static WSL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\bWsl/[A-Za-z]|\bWSL_E_[A-Z_]+|no installed distributions|there is no distribution with the supplied name|windows subsystem for linux (has no|instance has terminated)",
    )
    .expect("WSL_RE")
});

static NETWORK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(EAI_AGAIN|ENOTFOUND|ENETUNREACH|EHOSTUNREACH|ECONNREFUSED|ETIMEDOUT|ECONNRESET)\b|network is unreachable|temporary failure in name resolution|could not resolve host|dns error",
    )
    .expect("NETWORK_RE")
});

/// `wsl.exe` writes its own errors as UTF-16LE; read lossily as UTF-8 they
/// come out with a NUL between every character and match nothing.
fn normalise(text: &str) -> String {
    text.replace('\0', "")
}

/// The infrastructure cause named in `text`, if any. WSL is checked first:
/// a WSL start failure can mention networking without the network being the
/// cause.
pub fn classify(text: &str) -> Option<FailureClass> {
    let text = normalise(text);
    if WSL_RE.is_match(&text) {
        return Some(FailureClass::WslUnavailable);
    }
    NETWORK_RE.find(&text).map(|m| {
        let token = m.as_str();
        // Name the errno when there is one; otherwise the phrase is the token.
        FailureClass::Network(if token.starts_with('E') && token.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
            token.to_string()
        } else {
            token.to_ascii_lowercase()
        })
    })
}

/// [`classify`] over a process's combined stdout + stderr bytes.
pub fn classify_output(stdout: &[u8], stderr: &[u8]) -> Option<FailureClass> {
    let mut text = String::from_utf8_lossy(stderr).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(stdout));
    classify(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_dns_errno() {
        assert_eq!(
            classify("OAuth error: getaddrinfo EAI_AGAIN platform.claude.com"),
            Some(FailureClass::Network("EAI_AGAIN".into()))
        );
        assert_eq!(
            classify("curl: (6) Could not resolve host: example.com"),
            Some(FailureClass::Network("could not resolve host".into()))
        );
    }

    #[test]
    fn reads_utf16_wsl_errors() {
        let utf16_ish: String = "Error code: Wsl/Service/CreateInstance/E_FAIL"
            .chars()
            .flat_map(|c| [c, '\0'])
            .collect();
        assert_eq!(classify(&utf16_ish), Some(FailureClass::WslUnavailable));
        assert_eq!(
            classify("Windows Subsystem for Linux has no installed distributions."),
            Some(FailureClass::WslUnavailable)
        );
    }

    #[test]
    fn wsl_outranks_network() {
        assert_eq!(
            classify("Wsl/Service/CreateInstance/CreateVm/ConfigureNetworking/0x8007054f ETIMEDOUT"),
            Some(FailureClass::WslUnavailable)
        );
    }

    #[test]
    fn ordinary_failures_are_unclassified() {
        assert_eq!(classify("Invalid API key · Please run /login"), None);
        assert_eq!(classify("error: unknown option '--foo'"), None);
        assert_eq!(classify(""), None);
    }
}
