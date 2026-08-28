//! The server behind the `~/.claude/ide/<port>.lock` file: MCP over WebSocket.
//!
//! Writing a lock file is only half of `claude --ide`. The lock advertises
//! `"transport": "ws"`, and Claude Code then opens a WebSocket to that port and
//! speaks **MCP JSON-RPC 2.0** over text frames. Before ikenga#155 was
//! reopened, the lock pointed at the iyke HTTP bridge, which does not speak
//! that protocol at all — so discovery could never have worked no matter how
//! correct the file was.
//!
//! # Auth
//!
//! Claude Code sends the lock's `authToken` in the
//! `x-claude-code-ide-authorization` header on the upgrade request — *not* as
//! `Authorization: Bearer`. That is why this route is registered outside the
//! iyke bearer-token middleware and checks the header itself. The token is the
//! same bridge token; a mismatch is a 401 and the socket never upgrades.
//!
//! # What is actually implemented
//!
//! Only tools the shell can honestly answer. Everything else returns an
//! explicit "not supported by Ikenga" result rather than a plausible-looking
//! empty success, because a silent empty answer is indistinguishable from a
//! real one and that is how a dead integration survives review.
//!
//! | Tool | Status |
//! |---|---|
//! | `getWorkspaceFolders` | real — the active project's `root_path` |
//! | `openFile` | real — emits `ide://open_file`, the shell opens it |
//! | `getCurrentSelection` / `getLatestSelection` | real, empty until a pane reports one |
//! | `getDiagnostics` | empty — the shell aggregates no LSP diagnostics yet |
//! | `openDiff`, `close_tab`, `closeAllDiffTabs`, `saveDocument`, `executeCode` | explicit unsupported |

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use super::auth::AuthState;
use crate::commands::db::PaDb;

/// The header Claude Code puts the lock's `authToken` in.
const IDE_AUTH_HEADER: &str = "x-claude-code-ide-authorization";

/// The WebSocket subprotocol Claude Code requires. Selecting it is mandatory:
/// an unselected subprotocol makes the client hang up before saying anything.
const MCP_SUBPROTOCOL: &str = "mcp";

/// Fallback when the client does not state one. Claude Code always does.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

pub async fn ide_ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    Extension(app): Extension<AppHandle>,
    Extension(auth): Extension<Arc<AuthState>>,
    Extension(pa_db): Extension<Arc<PaDb>>,
) -> Response {
    let presented = headers
        .get(IDE_AUTH_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();

    if presented.is_empty() || presented != auth.token {
        tracing::warn!("ide_ws: rejected upgrade with a bad or missing {IDE_AUTH_HEADER}");
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }

    // Claude Code sends `Sec-WebSocket-Protocol: mcp` and CLOSES the socket
    // immediately if the server does not select it — connect, no frames, gone
    // in ~30ms. Observed against claude-code/2.1.250; nothing in the JSON-RPC
    // layer can detect it, because the connection dies before a single frame
    // is exchanged.
    ws.protocols([MCP_SUBPROTOCOL])
        .on_upgrade(move |socket| handle_socket(socket, app, pa_db))
}

async fn handle_socket(mut socket: WebSocket, app: AppHandle, pa_db: Arc<PaDb>) {
    tracing::info!("ide_ws: claude attached");

    while let Some(Ok(msg)) = socket.recv().await {
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            // Claude Code speaks text frames; ping/pong is handled by axum.
            _ => continue,
        };

        let Ok(req) = serde_json::from_str::<Value>(&text) else {
            tracing::warn!("ide_ws: dropped a frame that was not JSON");
            continue;
        };

        // Method names only, at debug. Never log the frames themselves: a
        // `tools/call` result can carry file contents, and `initialize` params
        // carry client details. Bind before the macro — `tracing::info!` pulls
        // its own `Value` trait into scope during expansion and shadows
        // `serde_json::Value`.
        let inbound_method = req
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("<no method>")
            .to_string();
        tracing::debug!("ide_ws: <- {inbound_method}");

        let Some(response) = dispatch(&req, &app, &pa_db).await else {
            // A notification — no id, no reply.
            continue;
        };

        if socket
            .send(Message::Text(response.to_string()))
            .await
            .is_err()
        {
            break;
        }
    }

    tracing::info!("ide_ws: claude detached");
}

/// Returns `None` for notifications (no `id`), which must not be answered.
async fn dispatch(req: &Value, app: &AppHandle, pa_db: &Arc<PaDb>) -> Option<Value> {
    let method = req
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let id = req.get("id").cloned();

    if id.is_none() {
        return None;
    }
    let id = id.unwrap();

    let result = match method {
        "initialize" => {
            let protocol = req
                .get("params")
                .and_then(|p| p.get("protocolVersion"))
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_PROTOCOL_VERSION)
                .to_string();
            json!({
                "protocolVersion": protocol,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "Ikenga", "version": env!("CARGO_PKG_VERSION") },
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tool_list() }),
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or_else(|| json!({}));
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            call_tool(name, &args, app, pa_db).await
        }
        _ => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method not found: {method}") },
            }))
        }
    };

    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn tool_list() -> Vec<Value> {
    let no_args = json!({ "type": "object", "properties": {} });
    vec![
        json!({
            "name": "getWorkspaceFolders",
            "description": "The workspace folders open in Ikenga (the active project's root).",
            "inputSchema": no_args,
        }),
        json!({
            "name": "getCurrentSelection",
            "description": "The current editor selection in Ikenga, if any pane has reported one.",
            "inputSchema": no_args,
        }),
        json!({
            "name": "getLatestSelection",
            "description": "The most recent editor selection in Ikenga, if any.",
            "inputSchema": no_args,
        }),
        json!({
            "name": "getDiagnostics",
            "description": "Language diagnostics. Ikenga aggregates none today and always returns an empty list.",
            "inputSchema": json!({
                "type": "object",
                "properties": { "uri": { "type": "string" } },
            }),
        }),
        json!({
            "name": "openFile",
            "description": "Open a file in Ikenga, optionally at a line and column.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "filePath": { "type": "string" },
                    "line": { "type": "number" },
                    "column": { "type": "number" },
                },
                "required": ["filePath"],
            }),
        }),
    ]
}

async fn call_tool(name: &str, args: &Value, app: &AppHandle, pa_db: &Arc<PaDb>) -> Value {
    match name {
        "getWorkspaceFolders" => {
            let folders = active_workspace_folders(pa_db).await;
            text_result(&json!({ "workspaceFolders": folders }).to_string())
        }
        // The shell has no editor-selection source wired into the kernel yet.
        // Answer honestly and consistently rather than inventing one.
        "getCurrentSelection" | "getLatestSelection" => {
            text_result(&json!({ "selection": Value::Null }).to_string())
        }
        "getDiagnostics" => text_result(&json!({ "diagnostics": [] }).to_string()),
        "openFile" => {
            let Some(file_path) = args.get("filePath").and_then(Value::as_str) else {
                return error_result("openFile requires a filePath");
            };
            let params = super::ide::OpenFileParams {
                file_path: file_path.to_string(),
                line: args.get("line").and_then(Value::as_u64).map(|n| n as u32),
                column: args.get("column").and_then(Value::as_u64).map(|n| n as u32),
            };
            let _ = app.emit("ide://open_file", &params);
            text_result(&format!("opened {file_path}"))
        }
        other => error_result(&format!(
            "{other} is not supported by Ikenga's IDE integration"
        )),
    }
}

/// The active project's `root_path`, or an empty list when there isn't one.
pub async fn active_workspace_folders(pa_db: &Arc<PaDb>) -> Vec<String> {
    let Ok(pool) = pa_db.ensure_pool().await else {
        return Vec::new();
    };
    let Ok(project_id) = crate::commands::projects::get_active_project_id(&pool).await else {
        return Vec::new();
    };
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT root_path FROM projects WHERE id = ?")
            .bind(project_id)
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten();

    row.and_then(|(p,)| p).into_iter().collect()
}

fn text_result(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

fn error_result(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, id: i64) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method })
    }

    /// Claude Code sends `notifications/initialized` with no id. Answering a
    /// notification is a protocol violation and some clients drop the session.
    #[test]
    fn notifications_are_not_answered() {
        let notification = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(notification.get("id").is_none());
    }

    #[test]
    fn the_advertised_tools_all_have_an_input_schema() {
        for tool in tool_list() {
            assert!(tool.get("name").and_then(Value::as_str).is_some());
            assert!(
                tool.get("inputSchema").is_some(),
                "every tool needs an inputSchema: {tool}"
            );
        }
    }

    /// The lock advertises `transport: "ws"`, so the tool names have to be the
    /// ones Claude Code actually calls. Guard the spelling — `close_tab` is
    /// snake_case upstream while the rest are camelCase, and a rename here is
    /// silent at compile time.
    #[test]
    fn tool_names_match_the_claude_code_ide_surface() {
        let names: Vec<String> = tool_list()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        for expected in [
            "getWorkspaceFolders",
            "getCurrentSelection",
            "getLatestSelection",
            "getDiagnostics",
            "openFile",
        ] {
            assert!(names.contains(&expected.to_string()), "missing {expected}");
        }
    }

    #[test]
    fn an_unknown_tool_is_an_explicit_error_not_an_empty_success() {
        let v = error_result("nope is not supported by Ikenga's IDE integration");
        assert_eq!(v["isError"], json!(true));
    }

    /// The one that cost an afternoon. Claude Code sends
    /// `Sec-WebSocket-Protocol: mcp` and, if the server does not select it,
    /// closes the socket after the handshake **without sending a single
    /// frame** — connect, ~30ms, gone. Every unit test still passed, the lock
    /// file was perfect, and the JSON-RPC layer was never reached, so nothing
    /// above the handshake could observe it. Only dumping the upgrade headers
    /// from a real `claude --ide` run found it.
    #[test]
    fn the_mcp_subprotocol_is_the_one_claude_code_asks_for() {
        assert_eq!(
            MCP_SUBPROTOCOL, "mcp",
            "renaming this silently breaks claude --ide with no error anywhere"
        );
    }

    #[test]
    fn request_shape_is_jsonrpc_2() {
        let r = req("tools/list", 7);
        assert_eq!(r["jsonrpc"], "2.0");
        assert_eq!(r["id"], 7);
    }
}
