//! Subagent format transcoder — ADR-012 §5 Rust port of the TS
//! `subagent-transcoder.ts` in `@ikenga/contract`.
//!
//! Translates between the canonical Markdown + YAML-frontmatter shape used
//! by Claude Code and Gemini CLI, and the TOML shape used by Codex CLI.
//! Zero external deps beyond `serde_yaml` (already a runtime dep) for
//! parsing the frontmatter — the TOML emit is hand-rolled to match the TS
//! emitter byte-for-byte where it matters for idempotency.
//!
//! Supported keys (top-level), per ADR §5:
//!   - Canonical:        name, description, tools, model, system_prompt
//!   - Codex extras:     developer_instructions, sandbox_mode, mcp_servers,
//!                       skills
//!   - Claude/Gemini extras: temperature, max_turns, timeout_mins (passed
//!                           through verbatim — Codex ignores them).
//!
//! Nested-table support is limited to `[mcp_servers]` (one level deep) —
//! that's the only realistic surface for subagents per the TS counterpart.
//!
//! ENTRY POINTS:
//!   - `md_to_codex_toml(md)`     — used by `engine_adapters::codex`.
//!   - `md_to_gemini_command_toml(md)` — reserved for the Gemini adapter
//!     (Track G). Compiled but currently `#[allow(dead_code)]` until Track
//!     G wires it in.
//!
//! Track G coordination: this module is shared by both adapters. Add new
//! functions by APPENDING at the bottom; do NOT reorder or remove existing
//! entries (per the ADR-012 phase 6 brief).

use anyhow::{anyhow, Result};
use serde_yaml::Value as YamlValue;

/// Ordered key→value list. We avoid `indexmap` (not in workspace deps) and
/// instead rely on `serde_yaml::Mapping` preserving insertion order.
type OrderedFields = Vec<(String, YamlValue)>;

/// MD → Codex TOML. The body becomes `system_prompt` (unless the
/// frontmatter already has one, in which case the body wins — matches the
/// TS convention).
pub fn md_to_codex_toml(md: &str) -> Result<String> {
    let (mut fm, body) = parse_frontmatter(md)?;
    if !body.is_empty() {
        upsert(&mut fm, "system_prompt", YamlValue::String(body));
    }
    Ok(emit_toml(&fm))
}

/// MD → Gemini slash-command TOML. The body becomes `prompt` (not
/// `system_prompt`). Reserved for Track G's `gemini.rs`.
#[allow(dead_code)]
pub fn md_to_gemini_command_toml(md: &str) -> Result<String> {
    let (mut fm, body) = parse_frontmatter(md)?;
    if !body.is_empty() {
        upsert(&mut fm, "prompt", YamlValue::String(body));
    }
    Ok(emit_toml(&fm))
}

fn upsert(fields: &mut OrderedFields, key: &str, value: YamlValue) {
    for (k, v) in fields.iter_mut() {
        if k == key {
            *v = value;
            return;
        }
    }
    fields.push((key.to_string(), value));
}

/// Parse YAML frontmatter + extract Markdown body. Returns
/// `(top-level-keys, body)`. No frontmatter → empty + entire md as body.
fn parse_frontmatter(md: &str) -> Result<(OrderedFields, String)> {
    let lines = md.split('\n').collect::<Vec<_>>();
    if lines.is_empty() || lines[0].trim() != "---" {
        return Ok((Vec::new(), md.to_string()));
    }
    let mut close_idx: Option<usize> = None;
    for (i, l) in lines.iter().enumerate().skip(1) {
        if l.trim() == "---" {
            close_idx = Some(i);
            break;
        }
    }
    let Some(close_idx) = close_idx else {
        return Err(anyhow!(
            "subagent transcoder: unterminated YAML frontmatter (missing closing `---`)"
        ));
    };
    let yaml_block = lines[1..close_idx].join("\n");
    // Body: skip one leading blank line between `---` and body (TS shape).
    let body_start = if lines.get(close_idx + 1).is_some_and(|s| s.is_empty()) {
        close_idx + 2
    } else {
        close_idx + 1
    };
    let body_lines = if body_start >= lines.len() {
        &[][..]
    } else {
        &lines[body_start..]
    };
    let body = body_lines.join("\n");

    let parsed: YamlValue = if yaml_block.trim().is_empty() {
        YamlValue::Mapping(Default::default())
    } else {
        serde_yaml::from_str(&yaml_block)
            .map_err(|e| anyhow!("subagent transcoder: parse YAML frontmatter: {e}"))?
    };
    let mut out: OrderedFields = Vec::new();
    if let YamlValue::Mapping(m) = parsed {
        for (k, v) in m {
            if let YamlValue::String(key) = k {
                out.push((key, v));
            }
        }
    }
    Ok((out, body))
}

/// Hand-rolled TOML emitter — matches the TS `emitToml` shape for the keys
/// ADR §5 lists. Top-level scalars first, then one nested table per
/// `Mapping` value (only `[mcp_servers]` realistically).
fn emit_toml(data: &OrderedFields) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut nested: Vec<(String, &serde_yaml::Mapping)> = Vec::new();
    for (k, v) in data {
        if let YamlValue::Mapping(m) = v {
            nested.push((k.clone(), m));
            continue;
        }
        lines.push(format!("{k} = {}", emit_toml_value(v, k)));
    }
    for (name, table) in nested {
        lines.push(String::new());
        lines.push(format!("[{name}]"));
        for (tk, tv) in table {
            if let YamlValue::String(key) = tk {
                lines.push(format!("{key} = {}", emit_toml_value(tv, key)));
            }
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn emit_toml_value(v: &YamlValue, key: &str) -> String {
    match v {
        YamlValue::String(s) => {
            // Triple-quote for the prompt-bearing keys + anything multi-line.
            if key == "system_prompt"
                || key == "prompt"
                || key == "developer_instructions"
                || s.contains('\n')
            {
                // Mirror TS line 230: `"""` → `"\""\""` so re-parse survives.
                let escaped = s.replace("\"\"\"", "\"\\\"\"\\\"\"");
                format!("\"\"\"\n{escaped}\"\"\"")
            } else {
                emit_basic_string(s)
            }
        }
        YamlValue::Bool(b) => b.to_string(),
        YamlValue::Number(n) => n.to_string(),
        YamlValue::Null => "\"\"".to_string(),
        YamlValue::Sequence(seq) => {
            let parts: Vec<String> = seq.iter().map(|x| emit_toml_value(x, key)).collect();
            format!("[{}]", parts.join(", "))
        }
        YamlValue::Mapping(_) => {
            // Should not be reached — top-level Mappings are routed to nested
            // tables. Fall back to a safe quoted serialization.
            serde_yaml::to_string(v)
                .map(|s| emit_basic_string(s.trim()))
                .unwrap_or_else(|_| "\"\"".to_string())
        }
        YamlValue::Tagged(_) => "\"\"".to_string(),
    }
}

/// TOML basic-string quoting (double-quoted, with escapes).
fn emit_basic_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md_to_codex_toml_basic() {
        let md = "---\nname: planner\ndescription: Builds plans\nmodel: o4-mini\n---\n\nYou are a planner.\n";
        let toml = md_to_codex_toml(md).unwrap();
        assert!(toml.contains("name = \"planner\""));
        assert!(toml.contains("description = \"Builds plans\""));
        assert!(toml.contains("model = \"o4-mini\""));
        assert!(toml.contains("system_prompt = \"\"\""));
        assert!(toml.contains("You are a planner."));
    }

    #[test]
    fn md_to_codex_toml_no_frontmatter() {
        let toml = md_to_codex_toml("just body\nlines\n").unwrap();
        assert!(toml.contains("system_prompt = \"\"\""));
    }

    #[test]
    fn md_to_codex_toml_preserves_list() {
        let md = "---\nname: r\ndescription: x\ntools: [Read, Write]\n---\n\nbody";
        let toml = md_to_codex_toml(md).unwrap();
        assert!(toml.contains("tools = [\"Read\", \"Write\"]"));
    }
}
