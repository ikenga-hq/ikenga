//! Statusline sidecar ingest & snapshot store (WP-01).
//!
//! Ingests per-refresh statusline JSON from Claude Code CLI, broadcasts structured
//! events over the Tauri event bus (`statusline://snapshot`), and maintains a thread-safe
//! snapshot store for frontend HUD / state queries.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use axum::{
    extract::Query,
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

static SNAPSHOT_STORE: OnceLock<Arc<Mutex<HashMap<String, StatuslineSnapshot>>>> = OnceLock::new();

fn get_store() -> &'static Arc<Mutex<HashMap<String, StatuslineSnapshot>>> {
    SNAPSHOT_STORE.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelInfo {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default, rename = "display_name")]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RepoInfo {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceInfo {
    #[serde(default)]
    pub current_dir: Option<String>,
    #[serde(default)]
    pub project_dir: Option<String>,
    #[serde(default)]
    pub added_dirs: Vec<String>,
    #[serde(default)]
    pub git_worktree: Option<String>,
    #[serde(default)]
    pub repo: Option<RepoInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CostInfo {
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    #[serde(default)]
    pub total_duration_ms: Option<u64>,
    #[serde(default)]
    pub total_api_duration_ms: Option<u64>,
    #[serde(default)]
    pub total_lines_added: Option<u64>,
    #[serde(default)]
    pub total_lines_removed: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CurrentUsageInfo {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContextWindowInfo {
    #[serde(default)]
    pub total_input_tokens: Option<u64>,
    #[serde(default)]
    pub total_output_tokens: Option<u64>,
    #[serde(default)]
    pub context_window_size: Option<u64>,
    #[serde(default)]
    pub used_percentage: Option<f64>,
    #[serde(default)]
    pub remaining_percentage: Option<f64>,
    #[serde(default)]
    pub current_usage: Option<CurrentUsageInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EffortInfo {
    #[serde(default)]
    pub level: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ThinkingInfo {
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RateLimitItem {
    #[serde(default)]
    pub used_percentage: Option<f64>,
    #[serde(default)]
    pub resets_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RateLimitsInfo {
    #[serde(default)]
    pub five_hour: Option<RateLimitItem>,
    #[serde(default)]
    pub seven_day: Option<RateLimitItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VimInfo {
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentInfo {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PrInfo {
    #[serde(default)]
    pub number: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub review_state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorktreeInfo {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub original_cwd: Option<String>,
    #[serde(default)]
    pub original_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatuslineSnapshot {
    #[serde(default, rename = "ikenga_terminal_id")]
    pub ikenga_terminal_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub model: Option<ModelInfo>,
    #[serde(default)]
    pub workspace: Option<WorkspaceInfo>,
    #[serde(default)]
    pub cost: Option<CostInfo>,
    #[serde(default)]
    pub context_window: Option<ContextWindowInfo>,
    #[serde(default)]
    pub exceeds_200k_tokens: Option<bool>,
    #[serde(default)]
    pub effort: Option<EffortInfo>,
    #[serde(default)]
    pub thinking: Option<ThinkingInfo>,
    #[serde(default)]
    pub rate_limits: Option<RateLimitsInfo>,
    #[serde(default)]
    pub vim: Option<VimInfo>,
    #[serde(default)]
    pub agent: Option<AgentInfo>,
    #[serde(default)]
    pub pr: Option<PrInfo>,
    #[serde(default)]
    pub worktree: Option<WorktreeInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StatuslineEventQuery {
    pub terminal: Option<String>,
}

/// Post route handler: POST /iyke/statusline/event
pub async fn post_statusline_event(
    Extension(app): Extension<AppHandle>,
    Query(query): Query<StatuslineEventQuery>,
    Json(mut snapshot): Json<StatuslineSnapshot>,
) -> impl IntoResponse {
    if snapshot.ikenga_terminal_id.is_none() {
        snapshot.ikenga_terminal_id = query.terminal.clone();
    }

    let terminal_id = snapshot.ikenga_terminal_id.clone().unwrap_or_default();
    if let Ok(mut store) = get_store().lock() {
        store.insert(terminal_id, snapshot.clone());
    }

    let _ = app.emit("statusline://snapshot", &snapshot);

    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" })))
}

/// Get route handler: GET /iyke/statusline/snapshot
///
/// Returns the full per-terminal map. Callers pick their own terminal's
/// snapshot by `ikenga_terminal_id`.
pub async fn get_statusline_snapshot() -> impl IntoResponse {
    let snapshots = get_store().lock().ok().map(|guard| guard.clone()).unwrap_or_default();

    (StatusCode::OK, Json(snapshots))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_statusline_snapshot_defensive_parse() {
        let json_str = r#"{
            "session_id": "test-session-123",
            "model": { "id": "claude-3-5-sonnet", "display_name": "Sonnet 3.5" },
            "cost": { "total_cost_usd": 0.05 },
            "context_window": { "used_percentage": 15.5 }
        }"#;

        let snapshot: StatuslineSnapshot = serde_json::from_str(json_str).expect("valid parse");
        assert_eq!(snapshot.session_id.as_deref(), Some("test-session-123"));
        assert_eq!(
            snapshot.model.as_ref().and_then(|m| m.display_name.as_deref()),
            Some("Sonnet 3.5")
        );
        assert_eq!(
            snapshot.cost.as_ref().and_then(|c| c.total_cost_usd),
            Some(0.05)
        );
    }
}
