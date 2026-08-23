//! Static table of coding-agent CLIs the wizard knows how to probe.
//!
//! Keep this list tight — every entry shows up in the wizard UI. Add new
//! agents only when they ship a real CLI we can shell out to. Anything that
//! requires a network probe to determine auth should be escalated rather
//! than added here (the wizard runs these synchronously and we don't want
//! to block on flaky internet).

use serde::Serialize;

/// Capability shape mirrors the engine adapter's `AdapterCapabilities` so
/// the wizard can render a green-check matrix without a separate type.
/// Hand-maintained today; future revisions will pull this from the engine
/// pkg manifest once one is installed.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct AgentCapabilities {
    pub streaming: bool,
    pub tool_use: bool,
    pub thinking: bool,
    pub artifacts: bool,
    pub mcp: bool,
    pub session_resume: bool,
}

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub enum TargetFamily {
    Any,
    Unix,
    Windows,
    Macos,
}

/// Per-target executable-resolution hint. The detect logic tries `names`
/// against `which` first, then falls back to scanning `extra_dirs`.
#[derive(Clone, Copy, Debug)]
pub struct ExecutableSpec {
    pub target_family: TargetFamily,
    pub names: &'static [&'static str],
    /// Extra dirs to search after PATH (e.g. `~/.cursor/bin`). Tilde is
    /// expanded at lookup time against `$HOME`.
    pub extra_dirs: &'static [&'static str],
}

/// How to determine whether the user has already authenticated this agent.
/// Slow / networked probes are explicitly forbidden — see escalation rules
/// in the phase doc.
#[derive(Clone, Copy, Debug)]
pub enum AuthCheck {
    /// Spawn `cmd` with `args`; exit-code 0 means authed. Wrapped in
    /// `tokio::time::timeout(timeout_ms)` so a hanging probe can't stall
    /// the wizard.
    Exec {
        cmd: &'static str,
        args: &'static [&'static str],
        timeout_ms: u64,
    },
    /// Presence of a non-empty env var implies authed.
    EnvVar { name: &'static str },
    /// Presence of any of these files (first match wins) implies authed.
    /// Paths support `~` expansion.
    FilePresent { paths: &'static [&'static str] },
    /// Truthy if *any* of the nested checks succeed.
    Any { checks: &'static [AuthCheck] },
    /// Spawn an ACP CLI (e.g. `gemini --acp`), run the `initialize` +
    /// `session/new` handshake, and read auth state from the protocol:
    /// `session/new` returning a result ⇒ authed; a JSON-RPC `-32000`
    /// (`AuthRequired`) error ⇒ not authed. This is the **authoritative**
    /// signal for ACP-native engines (ADR-013 §Addendum Decision 1) — robust
    /// against auth tokens stored anywhere a `FilePresent` probe can't
    /// enumerate. Wrapped in a `timeout_ms`. Inconclusive (`None`) on spawn
    /// failure / timeout so a `FirstConclusive` wrapper can fall back.
    AcpHandshake {
        args: &'static [&'static str],
        timeout_ms: u64,
    },
    /// Return the result of the first nested check that is *conclusive*
    /// (`Some(true)` or `Some(false)`); only fall through to the next when a
    /// check is inconclusive (`None`). Unlike `Any` (which OR-s and lets a
    /// stale file override a negative protocol probe), this preserves the
    /// authority of an earlier conclusive check — used to put `AcpHandshake`
    /// ahead of cred-file/env fallbacks (ADR-013 §Addendum Decision 1).
    FirstConclusive { checks: &'static [AuthCheck] },
}

#[derive(Clone, Copy, Debug)]
pub struct AgentDef {
    pub id: &'static str,
    pub display: &'static str,
    pub executables: &'static [ExecutableSpec],
    pub version_arg: Option<&'static str>,
    /// Captures one group; first match wins.
    pub version_regex: Option<&'static str>,
    pub auth_check: Option<AuthCheck>,
    pub capabilities: AgentCapabilities,
}

const CAP_FULL: AgentCapabilities = AgentCapabilities {
    streaming: true,
    tool_use: true,
    thinking: true,
    artifacts: true,
    mcp: true,
    session_resume: true,
};

// We PTY-wrap codex, so we surface the lowest common denominator until/unless
// we switch to `npx @zed-industries/codex-acp`. The CLI _does_ have richer
// capabilities natively, but the TUI byte stream gives us no reliable way to
// extract structured tool calls or thinking blocks — only the rendered text.
// If/when we replace the PTY-wrap with the Zed ACP adapter, restore the
// previous `tool_use: true, thinking: true` line.
const CAP_CODEX: AgentCapabilities = AgentCapabilities {
    streaming: true,
    tool_use: false, // PTY-wrap can't extract tool calls reliably
    thinking: false, // No structured signal through TUI
    artifacts: false,
    mcp: false,
    session_resume: false,
};

const CAP_GEMINI: AgentCapabilities = AgentCapabilities {
    streaming: true,
    tool_use: true,
    thinking: false,
    artifacts: false,
    mcp: false,
    session_resume: false,
};

const CAP_ANTIGRAVITY: AgentCapabilities = AgentCapabilities {
    streaming: false,
    tool_use: true,
    thinking: true,
    artifacts: true,
    mcp: true,
    session_resume: true,
};

const CAP_CURSOR: AgentCapabilities = AgentCapabilities {
    streaming: true,
    tool_use: true,
    thinking: false,
    artifacts: false,
    mcp: true,
    session_resume: false,
};

const CAP_OPENCODE: AgentCapabilities = AgentCapabilities {
    streaming: true,
    tool_use: true,
    thinking: false,
    artifacts: false,
    mcp: false,
    session_resume: false,
};

const CAP_QWEN: AgentCapabilities = AgentCapabilities {
    streaming: true,
    tool_use: true,
    thinking: false,
    artifacts: false,
    mcp: false,
    session_resume: false,
};

const CAP_AIDER: AgentCapabilities = AgentCapabilities {
    streaming: true,
    tool_use: true,
    thinking: false,
    artifacts: false,
    mcp: false,
    session_resume: false,
};

const CAP_OLLAMA: AgentCapabilities = AgentCapabilities {
    streaming: true,
    tool_use: false,
    thinking: false,
    artifacts: false,
    mcp: false,
    session_resume: false,
};

/// Standard semver-ish version regex, intentionally lenient so we catch
/// strings like `1.2.3`, `v1.2.3`, `1.2.3-rc.4`, `1.2.3+meta`. We don't
/// validate beyond "at least major.minor.patch" because CLIs ship a wide
/// variety of formats.
pub const DEFAULT_VERSION_REGEX: &str = r"(?i)v?(\d+\.\d+(?:\.\d+)?(?:[-+][0-9A-Za-z\.\-]+)?)";

pub const KNOWN_AGENTS: &[AgentDef] = &[
    AgentDef {
        id: "claude-code",
        display: "Claude Code",
        executables: &[
            ExecutableSpec {
                target_family: TargetFamily::Unix,
                names: &["claude"],
                extra_dirs: &[],
            },
            ExecutableSpec {
                target_family: TargetFamily::Windows,
                names: &["claude", "claude.cmd", "claude.exe", "claude.bat"],
                extra_dirs: &[],
            },
        ],
        version_arg: Some("--version"),
        version_regex: Some(DEFAULT_VERSION_REGEX),
        auth_check: Some(AuthCheck::Exec {
            cmd: "claude",
            args: &["doctor"],
            timeout_ms: 4000,
        }),
        capabilities: CAP_FULL,
    },
    AgentDef {
        id: "codex",
        display: "OpenAI Codex CLI",
        executables: &[
            ExecutableSpec {
                target_family: TargetFamily::Unix,
                names: &["codex"],
                extra_dirs: &[],
            },
            ExecutableSpec {
                target_family: TargetFamily::Windows,
                names: &["codex", "codex.cmd", "codex.exe", "codex.bat"],
                extra_dirs: &[],
            },
        ],
        version_arg: Some("--version"),
        version_regex: Some(DEFAULT_VERSION_REGEX),
        // `codex login` (the canonical ChatGPT/Codex flow) writes its token to
        // `~/.codex/auth.json` (mode 600). The bare `OPENAI_API_KEY` env-var
        // path covers users who plug in an API key directly. Same false-
        // negative class the gemini fix tackled — without the file check, a
        // freshly-logged-in user shows as "OPENAI_API_KEY not set" even though
        // `codex login status` reports `Logged in`.
        auth_check: Some(AuthCheck::Any {
            checks: &[
                AuthCheck::FilePresent {
                    paths: &["~/.codex/auth.json"],
                },
                AuthCheck::EnvVar {
                    name: "OPENAI_API_KEY",
                },
            ],
        }),
        capabilities: CAP_CODEX,
    },
    AgentDef {
        id: "gemini-cli",
        display: "Gemini CLI",
        executables: &[
            ExecutableSpec {
                target_family: TargetFamily::Unix,
                names: &["gemini"],
                extra_dirs: &[],
            },
            ExecutableSpec {
                target_family: TargetFamily::Windows,
                names: &["gemini", "gemini.cmd", "gemini.exe", "gemini.bat"],
                extra_dirs: &[],
            },
        ],
        version_arg: Some("--version"),
        version_regex: Some(DEFAULT_VERSION_REGEX),
        // ADR-013 §Addendum Decision 1: the ACP handshake is authoritative for
        // this ACP-native engine — `gemini --acp` + `session/new` reports auth
        // state directly, so we don't have to guess where the token lives. The
        // cred-file / env-var checks remain only as a fast fallback for when
        // the handshake can't run (spawn failure / timeout). `FirstConclusive`
        // (not `Any`) keeps the handshake's verdict authoritative — a stale
        // cred file can't flip a negative protocol probe back to "authed".
        auth_check: Some(AuthCheck::FirstConclusive {
            checks: &[
                // `--debug` is REQUIRED: gemini block-buffers stdout without
                // it, so the `session/new` response never flushes before our
                // timeout (ADR-013 §Probe findings). The probe tolerates the
                // extra debug lines — it skips anything that isn't the id:2
                // JSON-RPC response.
                AuthCheck::AcpHandshake {
                    args: &["--acp", "--debug"],
                    timeout_ms: 6000,
                },
                // oauth-personal writes `~/.gemini/oauth_creds.json`; the older
                // `~/.config/gemini/credentials.json` covers API-key-file
                // installs. Fallback only — used when the handshake is
                // inconclusive.
                AuthCheck::FilePresent {
                    paths: &[
                        "~/.gemini/oauth_creds.json",
                        "~/.config/gemini/credentials.json",
                    ],
                },
                AuthCheck::EnvVar {
                    name: "GEMINI_API_KEY",
                },
            ],
        }),
        capabilities: CAP_GEMINI,
    },
    AgentDef {
        id: "antigravity-cli",
        display: "Antigravity CLI",
        executables: &[
            ExecutableSpec {
                target_family: TargetFamily::Unix,
                names: &["agy"],
                extra_dirs: &["~/.local/bin"],
            },
            ExecutableSpec {
                target_family: TargetFamily::Windows,
                names: &["agy", "agy.cmd", "agy.exe", "agy.bat"],
                extra_dirs: &[],
            },
        ],
        version_arg: Some("--version"),
        version_regex: Some(DEFAULT_VERSION_REGEX),
        auth_check: Some(AuthCheck::Any {
            checks: &[
                AuthCheck::EnvVar {
                    name: "GEMINI_API_KEY",
                },
            ],
        }),
        capabilities: CAP_ANTIGRAVITY,
    },
    AgentDef {
        id: "cursor-agent",
        display: "Cursor Agent",
        executables: &[
            ExecutableSpec {
                target_family: TargetFamily::Unix,
                names: &["cursor-agent"],
                extra_dirs: &["~/.cursor/bin"],
            },
            ExecutableSpec {
                target_family: TargetFamily::Windows,
                names: &["cursor-agent", "cursor-agent.cmd", "cursor-agent.exe"],
                extra_dirs: &[],
            },
        ],
        version_arg: Some("--version"),
        version_regex: Some(DEFAULT_VERSION_REGEX),
        auth_check: None,
        capabilities: CAP_CURSOR,
    },
    AgentDef {
        id: "opencode",
        display: "OpenCode",
        executables: &[
            ExecutableSpec {
                target_family: TargetFamily::Unix,
                names: &["opencode"],
                extra_dirs: &[],
            },
            ExecutableSpec {
                target_family: TargetFamily::Windows,
                names: &["opencode", "opencode.cmd", "opencode.exe"],
                extra_dirs: &[],
            },
        ],
        version_arg: Some("--version"),
        version_regex: Some(DEFAULT_VERSION_REGEX),
        auth_check: None,
        capabilities: CAP_OPENCODE,
    },
    AgentDef {
        id: "qwen-code",
        display: "Qwen Code",
        executables: &[
            ExecutableSpec {
                target_family: TargetFamily::Unix,
                names: &["qwen"],
                extra_dirs: &[],
            },
            ExecutableSpec {
                target_family: TargetFamily::Windows,
                names: &["qwen", "qwen.cmd", "qwen.exe", "qwen.bat"],
                extra_dirs: &[],
            },
        ],
        version_arg: Some("--version"),
        version_regex: Some(DEFAULT_VERSION_REGEX),
        auth_check: Some(AuthCheck::EnvVar {
            name: "DASHSCOPE_API_KEY",
        }),
        capabilities: CAP_QWEN,
    },
    AgentDef {
        id: "aider",
        display: "Aider",
        executables: &[
            ExecutableSpec {
                target_family: TargetFamily::Unix,
                names: &["aider"],
                extra_dirs: &[],
            },
            ExecutableSpec {
                target_family: TargetFamily::Windows,
                names: &["aider", "aider.exe", "aider.cmd", "aider.bat"],
                extra_dirs: &[],
            },
        ],
        version_arg: Some("--version"),
        version_regex: Some(DEFAULT_VERSION_REGEX),
        auth_check: Some(AuthCheck::Any {
            checks: &[
                AuthCheck::EnvVar {
                    name: "OPENAI_API_KEY",
                },
                AuthCheck::EnvVar {
                    name: "ANTHROPIC_API_KEY",
                },
            ],
        }),
        capabilities: CAP_AIDER,
    },
    AgentDef {
        id: "ollama",
        display: "Ollama",
        executables: &[
            ExecutableSpec {
                target_family: TargetFamily::Unix,
                names: &["ollama"],
                extra_dirs: &[],
            },
            ExecutableSpec {
                target_family: TargetFamily::Windows,
                names: &["ollama", "ollama.exe"],
                extra_dirs: &[],
            },
        ],
        version_arg: Some("--version"),
        version_regex: Some(DEFAULT_VERSION_REGEX),
        auth_check: None,
        capabilities: CAP_OLLAMA,
    },
];

/// True if `spec.target_family` is `Any` or matches `os` (`std::env::consts::OS`).
pub fn family_matches(family: TargetFamily, os: &str) -> bool {
    match family {
        TargetFamily::Any => true,
        TargetFamily::Unix => !matches!(os, "windows"),
        TargetFamily::Macos => os == "macos",
        TargetFamily::Windows => os == "windows",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_agent_has_at_least_one_executable_for_some_target() {
        for def in KNOWN_AGENTS {
            assert!(
                !def.executables.is_empty(),
                "agent {} has no executables",
                def.id
            );
            for spec in def.executables {
                assert!(
                    !spec.names.is_empty(),
                    "agent {} has an ExecutableSpec with empty names",
                    def.id
                );
            }
        }
    }

    #[test]
    fn every_agent_has_unix_or_windows_coverage() {
        for def in KNOWN_AGENTS {
            let mut has_unix = false;
            let mut has_windows = false;
            for spec in def.executables {
                match spec.target_family {
                    TargetFamily::Any => {
                        has_unix = true;
                        has_windows = true;
                    }
                    TargetFamily::Unix | TargetFamily::Macos => has_unix = true,
                    TargetFamily::Windows => has_windows = true,
                }
            }
            assert!(
                has_unix || has_windows,
                "agent {} has no platform coverage",
                def.id
            );
        }
    }

    #[test]
    fn ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for def in KNOWN_AGENTS {
            assert!(seen.insert(def.id), "duplicate agent id: {}", def.id);
        }
    }

    #[test]
    fn default_version_regex_parses_common_outputs() {
        use regex::Regex;
        let re = Regex::new(DEFAULT_VERSION_REGEX).unwrap();
        let cases = [
            ("claude 1.2.3", "1.2.3"),
            ("codex CLI v0.4.1", "0.4.1"),
            ("gemini 1.0.0-rc.2", "1.0.0-rc.2"),
            ("cursor-agent 0.5.7", "0.5.7"),
            ("opencode v1.10.0", "1.10.0"),
            ("qwen 0.2", "0.2"),
            ("aider 0.85.1-dev", "0.85.1-dev"),
            ("ollama version is 0.5.4", "0.5.4"),
        ];
        for (input, expected) in cases {
            let caps = re.captures(input).unwrap_or_else(|| {
                panic!("regex did not match `{}`", input);
            });
            assert_eq!(caps.get(1).unwrap().as_str(), expected);
        }
    }

    #[test]
    fn family_matches_handles_runtime_os() {
        assert!(family_matches(TargetFamily::Any, "linux"));
        assert!(family_matches(TargetFamily::Any, "windows"));
        assert!(family_matches(TargetFamily::Unix, "linux"));
        assert!(family_matches(TargetFamily::Unix, "macos"));
        assert!(!family_matches(TargetFamily::Unix, "windows"));
        assert!(family_matches(TargetFamily::Macos, "macos"));
        assert!(!family_matches(TargetFamily::Macos, "linux"));
        assert!(family_matches(TargetFamily::Windows, "windows"));
        assert!(!family_matches(TargetFamily::Windows, "linux"));
    }
}
