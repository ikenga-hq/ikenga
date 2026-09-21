//! Transcript JSONL usage mirror and aggregator (WP-14 / DEC-24).
//!
//! Scans ~/.claude/projects/ JSONL transcripts incrementally using an mtime +
//! byte-offset watermark in `ngwa_transcript_files`, records usage events
//! in `ngwa_usage_events`, and computes 7d / 30d aggregates for NgwaSnapshot.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;



/// One attributed usage event to be inserted into SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageEvent {
    pub kind: String, // "skill" | "agent" | "tool" | "server"
    pub name: String,
    pub timestamp_ms: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
    pub session_id: Option<String>,
}

/// Raw usage aggregates computed across the mirror table.
#[derive(Debug, Clone, Default)]
pub struct RawUsageAggregate {
    pub last_used_ms: Option<i64>,
    pub count_7d: i64,
    pub count_30d: i64,
    pub tokens_30d: i64,
}

/// NgwaUsage wire type matching @ikenga/contract/ngwa.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NgwaUsageWire {
    pub source: String,
    pub last_used_ms: Option<i64>,
    pub count_7d: Option<i64>,
    pub count_30d: Option<i64>,
    pub tokens_30d: Option<i64>,
    pub window_start_ms: i64,
}

/// In-memory snapshot of all aggregated usage data.
#[derive(Debug, Clone)]
pub struct UsageSnapshot {
    pub aggregates: HashMap<(String, String), RawUsageAggregate>,
    pub window_start_ms: i64,
    pub total_events: usize,
}

impl UsageSnapshot {
    pub fn empty(now_ms: i64) -> Self {
        Self {
            aggregates: HashMap::new(),
            window_start_ms: now_ms.saturating_sub(90 * 86_400_000),
            total_events: 0,
        }
    }

    /// Query usage for a measurable primitive or tool.
    /// Returns a measured zero `{ count_7d: 0, ... }` if never used.
    pub fn for_primitive(&self, kind: &str, name: &str) -> NgwaUsageWire {
        if let Some(agg) = self.aggregates.get(&(kind.to_string(), name.to_string())) {
            NgwaUsageWire {
                source: "transcript".to_string(),
                last_used_ms: agg.last_used_ms,
                count_7d: Some(agg.count_7d),
                count_30d: Some(agg.count_30d),
                tokens_30d: Some(agg.tokens_30d),
                window_start_ms: self.window_start_ms,
            }
        } else {
            NgwaUsageWire {
                source: "transcript".to_string(),
                last_used_ms: None,
                count_7d: Some(0),
                count_30d: Some(0),
                tokens_30d: Some(0),
                window_start_ms: self.window_start_ms,
            }
        }
    }

    /// Query usage for a server name contributed by a pkg.
    pub fn for_server(&self, server_name: &str) -> Option<NgwaUsageWire> {
        if let Some(agg) = self
            .aggregates
            .get(&("server".to_string(), server_name.to_string()))
        {
            Some(NgwaUsageWire {
                source: "transcript".to_string(),
                last_used_ms: agg.last_used_ms,
                count_7d: Some(agg.count_7d),
                count_30d: Some(agg.count_30d),
                tokens_30d: Some(agg.tokens_30d),
                window_start_ms: self.window_start_ms,
            })
        } else {
            Some(NgwaUsageWire {
                source: "transcript".to_string(),
                last_used_ms: None,
                count_7d: Some(0),
                count_30d: Some(0),
                tokens_30d: Some(0),
                window_start_ms: self.window_start_ms,
            })
        }
    }
}

/// Returns current unix timestamp in milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Convert civil date/time to unix millis using Howard Hinnant's algorithm.
pub fn civil_to_unix_ms(
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    millis: u32,
) -> i64 {
    let y = year - if month <= 2 { 1 } else { 0 };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = (y - era * 400) as u32;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe as i64 - 719468;
    let seconds = days * 86400 + (hour as i64) * 3600 + (minute as i64) * 60 + (second as i64);
    seconds * 1000 + (millis as i64)
}

/// Parse ISO 8601 UTC timestamp `YYYY-MM-DDTHH:MM:SS[.mmm]Z` into unix millis.
pub fn parse_iso_timestamp(ts: &str) -> Option<i64> {
    let s = ts.trim();
    if s.len() < 19 {
        return None;
    }
    let parts: Vec<&str> = s.split('T').collect();
    if parts.len() != 2 {
        return None;
    }
    let date_parts: Vec<&str> = parts[0].split('-').collect();
    if date_parts.len() != 3 {
        return None;
    }
    let year: i64 = date_parts[0].parse().ok()?;
    let month: u32 = date_parts[1].parse().ok()?;
    let day: u32 = date_parts[2].parse().ok()?;

    let time_str = parts[1].trim_end_matches('Z');
    let time_subparts: Vec<&str> = time_str.split('.').collect();
    let hms: Vec<&str> = time_subparts[0].split(':').collect();
    if hms.len() != 3 {
        return None;
    }
    let hour: u32 = hms[0].parse().ok()?;
    let minute: u32 = hms[1].parse().ok()?;
    let second: u32 = hms[2].parse().ok()?;

    let millis: u32 = if time_subparts.len() > 1 {
        let ms_str = time_subparts[1];
        if ms_str.len() >= 3 {
            ms_str[..3].parse().unwrap_or(0)
        } else {
            let padded = format!("{:0<3}", ms_str);
            padded.parse().unwrap_or(0)
        }
    } else {
        0
    };

    Some(civil_to_unix_ms(
        year, month, day, hour, minute, second, millis,
    ))
}

/// Resolve the default Claude Code projects directory (`~/.claude/projects`).
pub fn claude_projects_dir() -> Option<PathBuf> {
    crate::platform::home_dir().map(|h| h.join(".claude").join("projects"))
}

/// Walk all `.jsonl` files under a root directory recursively.
pub fn discover_transcript_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk_dir_recursive(root, &mut files);
    files
}

fn walk_dir_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_dir_recursive(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

/// Read agentType from companion `agent-<id>.meta.json` if present.
fn read_subagent_meta_type(jsonl_path: &Path) -> Option<String> {
    let file_name = jsonl_path.file_name()?.to_str()?;
    if !file_name.starts_with("agent-") || !file_name.ends_with(".jsonl") {
        return None;
    }
    let meta_name = format!("{}.meta.json", &file_name[..file_name.len() - 6]);
    let meta_path = jsonl_path.with_file_name(meta_name);
    let raw = std::fs::read_to_string(&meta_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("agentType")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
}

/// Extract usage events from a single JSONL line.
pub fn extract_events_from_line(line: &str, meta_agent_type: Option<&str>) -> Vec<UsageEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut events = Vec::new();
    let Ok(raw_val) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Vec::new();
    };

    let record_type = raw_val.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if record_type != "assistant" {
        return Vec::new();
    }

    // Timestamp
    let ts_str = raw_val.get("timestamp").and_then(|t| t.as_str());
    let ts_ms = ts_str.and_then(parse_iso_timestamp).unwrap_or_else(now_ms);

    let session_id = raw_val
        .get("sessionId")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());

    // Tokens
    let msg_obj = raw_val.get("message");
    let usage_obj = msg_obj.and_then(|m| m.get("usage"));
    let input_tokens = usage_obj
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let output_tokens = usage_obj
        .and_then(|u| u.get("output_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let cache_creation_tokens = usage_obj
        .and_then(|u| u.get("cache_creation_input_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let cache_read_tokens = usage_obj
        .and_then(|u| u.get("cache_read_input_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    // 1. attributionSkill (turn-level skill attribution)
    if let Some(skill) = raw_val.get("attributionSkill").and_then(|s| s.as_str()) {
        if !skill.trim().is_empty() {
            events.push(UsageEvent {
                kind: "skill".to_string(),
                name: skill.to_string(),
                timestamp_ms: ts_ms,
                input_tokens,
                output_tokens,
                cache_creation_tokens,
                cache_read_tokens,
                session_id: session_id.clone(),
            });
        }
    }

    // 2. attributionAgent (turn-level agent attribution) or meta.json agentType
    let agent_name = raw_val
        .get("attributionAgent")
        .and_then(|s| s.as_str())
        .or(meta_agent_type);
    if let Some(agent) = agent_name {
        if !agent.trim().is_empty() {
            events.push(UsageEvent {
                kind: "agent".to_string(),
                name: agent.to_string(),
                timestamp_ms: ts_ms,
                input_tokens,
                output_tokens,
                cache_creation_tokens,
                cache_read_tokens,
                session_id: session_id.clone(),
            });
        }
    }

    // 3. ToolUse inside message content
    if let Some(contents) = msg_obj
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        for item in contents {
            let is_tool_use = item.get("type").and_then(|t| t.as_str()) == Some("tool_use");
            if !is_tool_use {
                continue;
            }
            let Some(name) = item.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let input = item.get("input");

            if name == "Skill" {
                if let Some(skill) = input.and_then(|inp| inp.get("skill")).and_then(|s| s.as_str()) {
                    events.push(UsageEvent {
                        kind: "skill".to_string(),
                        name: skill.to_string(),
                        timestamp_ms: ts_ms,
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_creation_tokens: 0,
                        cache_read_tokens: 0,
                        session_id: session_id.clone(),
                    });
                }
            } else if name == "Agent" || name == "Task" {
                if let Some(agent) = input
                    .and_then(|inp| inp.get("subagent_type"))
                    .and_then(|s| s.as_str())
                {
                    events.push(UsageEvent {
                        kind: "agent".to_string(),
                        name: agent.to_string(),
                        timestamp_ms: ts_ms,
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_creation_tokens: 0,
                        cache_read_tokens: 0,
                        session_id: session_id.clone(),
                    });
                }
            } else if name.starts_with("mcp__") {
                // Whole tool name: mcp__<server>__<tool>
                events.push(UsageEvent {
                    kind: "tool".to_string(),
                    name: name.to_string(),
                    timestamp_ms: ts_ms,
                    input_tokens,
                    output_tokens,
                    cache_creation_tokens,
                    cache_read_tokens,
                    session_id: session_id.clone(),
                });

                // Server attribution for pkgs contributing an MCP server
                let parts: Vec<&str> = name.split("__").collect();
                if parts.len() >= 3 {
                    let server_name = parts[1];
                    events.push(UsageEvent {
                        kind: "server".to_string(),
                        name: server_name.to_string(),
                        timestamp_ms: ts_ms,
                        input_tokens,
                        output_tokens,
                        cache_creation_tokens,
                        cache_read_tokens,
                        session_id: session_id.clone(),
                    });
                }
            }
        }
    }

    events
}

/// Run an incremental scan over all transcript files and mirror events to SQLite.
pub async fn scan_and_mirror_transcripts(
    pool: &SqlitePool,
    root: &Path,
) -> Result<usize, String> {
    if !root.is_dir() {
        return Ok(0);
    }

    // 1. Read existing watermarks
    let mut watermarks: HashMap<String, (i64, i64)> = HashMap::new();
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT path, mtime_ms, byte_offset FROM ngwa_transcript_files",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    for (p, mtime, off) in rows {
        watermarks.insert(p, (mtime, off));
    }

    // 2. Discover all jsonl files
    let files = discover_transcript_files(root);
    let mut total_new_events = 0;
    let now = now_ms();

    for path in files {
        let path_str = path.to_string_lossy().to_string();
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };

        let file_len = meta.len() as i64;
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        // Check cache
        let (cached_mtime, cached_offset) = watermarks.get(&path_str).copied().unwrap_or((0, 0));
        if cached_mtime == mtime_ms && cached_offset == file_len {
            // Unmodified file, skip!
            continue;
        }

        let start_offset = if file_len < cached_offset {
            0
        } else {
            cached_offset as u64
        };

        let Ok(mut file) = File::open(&path) else {
            continue;
        };
        if file.seek(SeekFrom::Start(start_offset)).is_err() {
            continue;
        }

        let meta_agent_type = read_subagent_meta_type(&path);
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        let mut new_events = Vec::new();

        while let Ok(bytes) = reader.read_line(&mut line) {
            if bytes == 0 {
                break;
            }
            let evs = extract_events_from_line(&line, meta_agent_type.as_deref());
            new_events.extend(evs);
            line.clear();
        }

        // Write events in a transaction
        if !new_events.is_empty() {
            total_new_events += new_events.len();
            let mut tx = pool.begin().await.map_err(|e| e.to_string())?;

            for ev in &new_events {
                sqlx::query(
                    r#"
                    INSERT INTO ngwa_usage_events (
                        kind, name, timestamp_ms, input_tokens, output_tokens,
                        cache_creation_tokens, cache_read_tokens, session_id
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                    "#,
                )
                .bind(&ev.kind)
                .bind(&ev.name)
                .bind(ev.timestamp_ms)
                .bind(ev.input_tokens)
                .bind(ev.output_tokens)
                .bind(ev.cache_creation_tokens)
                .bind(ev.cache_read_tokens)
                .bind(&ev.session_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
            }

            sqlx::query(
                r#"
                INSERT INTO ngwa_transcript_files (path, mtime_ms, byte_offset, scanned_at_ms)
                VALUES (?, ?, ?, ?)
                ON CONFLICT(path) DO UPDATE SET
                    mtime_ms = excluded.mtime_ms,
                    byte_offset = excluded.byte_offset,
                    scanned_at_ms = excluded.scanned_at_ms
                "#,
            )
            .bind(&path_str)
            .bind(mtime_ms)
            .bind(file_len)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;

            tx.commit().await.map_err(|e| e.to_string())?;
        } else {
            // Update watermark even if 0 events were produced
            let _ = sqlx::query(
                r#"
                INSERT INTO ngwa_transcript_files (path, mtime_ms, byte_offset, scanned_at_ms)
                VALUES (?, ?, ?, ?)
                ON CONFLICT(path) DO UPDATE SET
                    mtime_ms = excluded.mtime_ms,
                    byte_offset = excluded.byte_offset,
                    scanned_at_ms = excluded.scanned_at_ms
                "#,
            )
            .bind(&path_str)
            .bind(mtime_ms)
            .bind(file_len)
            .bind(now)
            .execute(pool)
            .await;
        }
    }

    Ok(total_new_events)
}

/// Load usage snapshot aggregating usage metrics from SQLite.
pub async fn load_usage_snapshot(pool: &SqlitePool, now_ms: i64) -> Result<UsageSnapshot, String> {
    let seven_days_ago = now_ms - 7 * 86_400_000;
    let thirty_days_ago = now_ms - 30 * 86_400_000;

    let rows: Vec<(String, String, Option<i64>, i64, i64, i64)> = sqlx::query_as(
        r#"
        SELECT
            kind,
            name,
            MAX(timestamp_ms) AS last_used_ms,
            SUM(CASE WHEN timestamp_ms >= ? THEN 1 ELSE 0 END) AS count_7d,
            SUM(CASE WHEN timestamp_ms >= ? THEN 1 ELSE 0 END) AS count_30d,
            SUM(CASE WHEN timestamp_ms >= ? THEN (input_tokens + output_tokens + cache_creation_tokens + cache_read_tokens) ELSE 0 END) AS tokens_30d
        FROM ngwa_usage_events
        GROUP BY kind, name
        "#,
    )
    .bind(seven_days_ago)
    .bind(thirty_days_ago)
    .bind(thirty_days_ago)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("load usage aggregates: {e}"))?;

    let mut aggregates = HashMap::new();
    for (kind, name, last_used_ms, c7, c30, tok30) in rows {
        aggregates.insert(
            (kind, name),
            RawUsageAggregate {
                last_used_ms,
                count_7d: c7,
                count_30d: c30,
                tokens_30d: tok30,
            },
        );
    }

    let min_ts: Option<i64> = sqlx::query_scalar("SELECT MIN(timestamp_ms) FROM ngwa_usage_events")
        .fetch_one(pool)
        .await
        .unwrap_or(None);

    let total_events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ngwa_usage_events")
        .fetch_one(pool)
        .await
        .unwrap_or(0);

    let default_window_start = now_ms - 90 * 86_400_000;
    let window_start_ms = min_ts.unwrap_or(default_window_start);

    Ok(UsageSnapshot {
        aggregates,
        window_start_ms,
        total_events: total_events as usize,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_iso_timestamp() {
        let ts = "2026-09-15T22:06:07.889Z";
        let parsed = parse_iso_timestamp(ts).expect("parse timestamp");
        assert!(parsed > 1_700_000_000_000); // Year > 2023

        let ts2 = "2026-09-15T22:06:07Z";
        let parsed2 = parse_iso_timestamp(ts2).expect("parse timestamp without millis");
        assert_eq!(parsed - parsed2, 889);
    }

    #[test]
    fn test_extract_skill_and_agent_events() {
        let line = r#"{
            "type": "assistant",
            "timestamp": "2026-09-15T22:06:07.889Z",
            "attributionSkill": "groundwork",
            "message": {
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 20,
                    "cache_creation_input_tokens": 5,
                    "cache_read_input_tokens": 15
                },
                "content": [
                    {
                        "type": "tool_use",
                        "name": "Skill",
                        "input": { "skill": "workflow-authoring" }
                    },
                    {
                        "type": "tool_use",
                        "name": "mcp__iyke__iyke_state",
                        "input": {}
                    }
                ]
            }
        }"#;

        let events = extract_events_from_line(line, None);
        assert_eq!(events.len(), 4);

        // 1. attributionSkill
        assert_eq!(events[0].kind, "skill");
        assert_eq!(events[0].name, "groundwork");
        assert_eq!(events[0].input_tokens, 10);
        assert_eq!(events[0].output_tokens, 20);

        // 2. Skill tool_use
        assert_eq!(events[1].kind, "skill");
        assert_eq!(events[1].name, "workflow-authoring");

        // 3. MCP tool_use (tool)
        assert_eq!(events[2].kind, "tool");
        assert_eq!(events[2].name, "mcp__iyke__iyke_state");

        // 4. MCP tool_use (server)
        assert_eq!(events[3].kind, "server");
        assert_eq!(events[3].name, "iyke");
    }

    #[test]
    fn test_usage_snapshot_for_primitive() {
        let now = 1_800_000_000_000;
        let mut snapshot = UsageSnapshot::empty(now);
        snapshot.aggregates.insert(
            ("skill".to_string(), "groundwork".to_string()),
            RawUsageAggregate {
                last_used_ms: Some(now - 1000),
                count_7d: 5,
                count_30d: 12,
                tokens_30d: 45000,
            },
        );

        let usage = snapshot.for_primitive("skill", "groundwork");
        assert_eq!(usage.count_7d, Some(5));
        assert_eq!(usage.count_30d, Some(12));
        assert_eq!(usage.tokens_30d, Some(45000));
        assert_eq!(usage.last_used_ms, Some(now - 1000));

        let unused = snapshot.for_primitive("skill", "unused-skill");
        assert_eq!(unused.count_7d, Some(0));
        assert_eq!(unused.count_30d, Some(0));
        assert_eq!(unused.tokens_30d, Some(0));
        assert_eq!(unused.last_used_ms, None);
    }
}
