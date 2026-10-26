//! Per-run tool policy for Claude Code Chi runs (`chi_run` `opts.toolPolicy`).
//!
//! A caller that serves a person with a *limited* toolset (the Mattermost
//! bridge, for one) cannot enforce that limit from a prompt: it has to be in
//! the argv the daemon hands `claude`. This module is that contract.
//!
//! # Wire shape (`opts.toolPolicy`, camelCase; snake_case spellings accepted)
//!
//! ```json
//! { "tools": ["Read", "Bash"],
//!   "allowedTools": ["Read", "Bash(./scripts/submit.sh:*)"],
//!   "disallowedTools": ["Bash(curl:*)"],
//!   "mode": "dontAsk",
//!   "strictMcp": true,
//!   "settingSources": [] }
//! ```
//!
//! Strict on purpose: an unknown field, a wrong type, a bad value, or an
//! explicit `null` is an error — a policy that fails to parse must never run
//! as "no policy".
//!
//! # What the CLI does with it (probed against claude 2.1.310)
//!
//! * `--tools <csv>` is the exact built-in set (`--tools ""` = none, no MCP
//!   either). A tool outside it does not exist for the model.
//! * `--permission-mode dontAsk` turns *anything that would prompt* into an
//!   immediate denial (`Permission to use X has been denied because Claude
//!   Code is running in don't ask mode`); no `control_request` is sent, so a
//!   run can never park in `awaiting_auth`. This works together with
//!   `--permission-prompt-tool stdio` (unchanged from a normal run).
//! * `--allowedTools` / `--disallowedTools` are variadic; each rule is passed
//!   as its own argv element and rules containing spaces, commas and parens
//!   stay whole. The prompt rides stdin, so the variadic flags never see it.
//! * `--strict-mcp-config` with no `--mcp-config` loads no MCP server;
//!   `--disable-slash-commands` drops skills and commands;
//!   `--setting-sources ""` loads no settings file (so no allow/ask rules or
//!   hooks from disk).
//! * Allow rules do **not** confine reads: read-only Bash commands (`ls`,
//!   `cat`) and Read run in `dontAsk` whatever the allow list says. The run's
//!   cwd is the read boundary, not this policy.
//!
//! The policy is stored with the run (`chi_cache.tool_policy`) and re-applied
//! on `chi_resume`; see `chi_exec::cache_load_policy`.

use serde::{Deserialize, Serialize};

/// The `/api/health` `capabilities` entry a daemon advertises when it applies
/// `opts.toolPolicy`. A caller that needs a policy refuses to start a run
/// against a daemon that does not list it.
pub const CAPABILITY_TOOL_POLICY: &str = "chi.toolPolicy";

/// Every capability the daemon advertises on `/api/health`.
pub const CAPABILITIES: &[&str] = &[CAPABILITY_TOOL_POLICY];

/// The only engine a policy applies to.
pub(crate) const POLICY_ENGINE: &str = "claude-code";

/// Most rules a list may carry, and the longest one. Bounds the argv.
const MAX_RULES: usize = 256;
const MAX_RULE_LEN: usize = 1024;

/// The permission mode a policy run uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyMode {
    /// `--permission-mode default`. A prompted tool would ask; a policy run
    /// has nobody to answer, so the reader fails the run closed the moment a
    /// permission request appears (see `claude_one_off_task`).
    #[serde(rename = "default")]
    Default,
    /// `--permission-mode dontAsk`: unlisted tools are denied immediately.
    #[serde(rename = "dontAsk")]
    DontAsk,
}

impl PolicyMode {
    pub(crate) fn as_claude_flag(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::DontAsk => "dontAsk",
        }
    }
}

/// A run's tool policy. See the module docs for the wire shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolPolicy {
    /// Exact built-in tool set (`--tools`). Empty = no tools.
    pub tools: Vec<String>,
    /// `--allowedTools` rules.
    #[serde(default, alias = "allowed_tools")]
    pub allowed_tools: Vec<String>,
    /// `--disallowedTools` rules.
    #[serde(default, alias = "disallowed_tools")]
    pub disallowed_tools: Vec<String>,
    pub mode: PolicyMode,
    /// Must be `true`: MCP servers never load on a policy run.
    #[serde(alias = "strict_mcp")]
    pub strict_mcp: bool,
    /// `--setting-sources`: `[]` or `["project"]`.
    #[serde(default, alias = "setting_sources")]
    pub setting_sources: Vec<String>,
}

impl ToolPolicy {
    /// Parse and validate a wire value. Any deviation is an error.
    pub fn from_value(v: &serde_json::Value) -> Result<Self, String> {
        let policy: Self = serde_json::from_value(v.clone())
            .map_err(|e| format!("invalid toolPolicy: {e}"))?;
        policy.validate()?;
        Ok(policy)
    }

    /// Re-parse a stored policy (`chi_cache.tool_policy`).
    pub(crate) fn from_stored(json: &str) -> Result<Self, String> {
        let v: serde_json::Value = serde_json::from_str(json)
            .map_err(|e| format!("stored toolPolicy is not JSON: {e}"))?;
        Self::from_value(&v)
    }

    /// The canonical JSON stored with the run.
    pub(crate) fn to_stored(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|e| format!("serialize toolPolicy: {e}"))
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.strict_mcp {
            return Err("invalid toolPolicy: `strictMcp` must be true".to_string());
        }
        if self.tools.len() > MAX_RULES {
            return Err(format!("invalid toolPolicy: more than {MAX_RULES} `tools`"));
        }
        for t in &self.tools {
            let ok = !t.is_empty()
                && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                // `--tools default` means "the default set": the opposite of a limit.
                && !t.eq_ignore_ascii_case("default");
            if !ok {
                return Err(format!(
                    "invalid toolPolicy: `tools` entry {t:?} must be a built-in tool name \
                     (letters, digits, `_`; not `default`)"
                ));
            }
        }
        check_rules("allowedTools", &self.allowed_tools)?;
        check_rules("disallowedTools", &self.disallowed_tools)?;
        let mut seen = std::collections::HashSet::new();
        for s in &self.setting_sources {
            if s != "project" {
                return Err(format!(
                    "invalid toolPolicy: `settingSources` entry {s:?} is not allowed \
                     (only \"project\")"
                ));
            }
            if !seen.insert(s.as_str()) {
                return Err("invalid toolPolicy: duplicate `settingSources` entry".to_string());
            }
        }
        Ok(())
    }

    /// The `claude` flags this policy adds after the base argv. Every flag is
    /// emitted unconditionally except the two rule lists, and each variadic
    /// flag is followed by another flag (or the end), never a positional.
    pub(crate) fn claude_args(&self) -> Vec<String> {
        let s = |v: &str| v.to_string();
        let mut args = vec![s("--tools"), self.tools.join(",")];
        if !self.allowed_tools.is_empty() {
            args.push(s("--allowedTools"));
            args.extend(self.allowed_tools.iter().cloned());
        }
        if !self.disallowed_tools.is_empty() {
            args.push(s("--disallowedTools"));
            args.extend(self.disallowed_tools.iter().cloned());
        }
        args.push(s("--strict-mcp-config"));
        args.push(s("--setting-sources"));
        args.push(self.setting_sources.join(","));
        args.push(s("--disable-slash-commands"));
        args
    }
}

fn check_rules(field: &str, rules: &[String]) -> Result<(), String> {
    if rules.len() > MAX_RULES {
        return Err(format!("invalid toolPolicy: more than {MAX_RULES} `{field}`"));
    }
    for r in rules {
        // A leading `-` would be read as a flag; control characters (newline
        // included) have no place in a rule.
        let ok = !r.is_empty()
            && r.len() <= MAX_RULE_LEN
            && r.trim() == r
            && !r.starts_with('-')
            && !r.chars().any(char::is_control);
        if !ok {
            return Err(format!(
                "invalid toolPolicy: `{field}` entry {r:?} is empty, padded, over {MAX_RULE_LEN} \
                 bytes, starts with `-`, or has control characters"
            ));
        }
    }
    Ok(())
}

/// Read `toolPolicy` / `tool_policy` out of a `chi_run` argument object.
/// `None` only when the key is absent; an explicit `null` is an error (a
/// caller bug must not quietly become an unrestricted run).
pub fn policy_from_args(src: &serde_json::Value) -> Result<Option<ToolPolicy>, String> {
    let v = ["toolPolicy", "tool_policy"]
        .iter()
        .find_map(|k| src.get(*k));
    match v {
        None => Ok(None),
        Some(serde_json::Value::Null) => {
            Err("invalid toolPolicy: must be an object, not null".to_string())
        }
        Some(v) => ToolPolicy::from_value(v).map(Some),
    }
}
