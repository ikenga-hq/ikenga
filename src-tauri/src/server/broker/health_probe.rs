//! The broker's side of `server_health`'s per-account figures
//! (`server::host_health::AccountLoads`).
//!
//! The broker cannot count the principals' processes itself (`ProtectProc=
//! invisible`, no `CAP_SYS_PTRACE`), so it asks each *running* child, on the
//! per-child token, the same internal arm the update flow uses
//! (`server_open_terminals`, which also reports the child's own `claude`
//! process count). It never launches a child and never waits on one that is
//! starting: an idle account is `running: false`, a child that does not
//! answer in [`CHILD_TIMEOUT`] contributes no numbers.

use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};

use super::proxy::PRINCIPAL_HEADER;
use super::BrokerState;
use crate::server::host_health::{AccountLoad, AccountLoads};
use crate::server::operator::accounts;

/// How long the broker waits for one child.
const CHILD_TIMEOUT: Duration = Duration::from_secs(2);
/// More accounts than this are summarised by the first N (oldest first).
const MAX_ACCOUNTS: usize = 200;

pub struct BrokerAccountLoads {
    state: Arc<BrokerState>,
}

impl BrokerAccountLoads {
    pub fn new(state: Arc<BrokerState>) -> Self {
        Self { state }
    }
}

impl AccountLoads for BrokerAccountLoads {
    fn loads(&self) -> BoxFuture<'static, Option<Vec<AccountLoad>>> {
        let state = self.state.clone();
        Box::pin(async move { account_loads(&state).await })
    }
}

async fn account_loads(state: &BrokerState) -> Option<Vec<AccountLoad>> {
    let accounts = {
        let mut conn = state.pool.acquire().await.ok()?;
        accounts::list(&mut conn).await.ok()?
    };
    let (endpoints, _partial) = state.children.running_endpoints();
    let calls = accounts
        .into_iter()
        .filter(|a| !a.is_disabled())
        .take(MAX_ACCOUNTS)
        .map(|a| {
            let endpoint = endpoints
                .iter()
                .find(|(id, _)| *id == a.principal_id)
                .map(|(_, ep)| ep.clone());
            let http = state.http.clone();
            async move {
                let Some(ep) = endpoint else {
                    return AccountLoad {
                        username: a.username,
                        running: false,
                        terminals: None,
                        claude_processes: None,
                    };
                };
                let sent = http
                    .post(format!("http://{}/api/rpc", ep.addr))
                    .bearer_auth(&*ep.token)
                    .header(PRINCIPAL_HEADER, a.principal_id.to_string())
                    .header(crate::access::INTERNAL_CALL_HEADER, "1")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(json!({ "cmd": "server_open_terminals", "args": {} }).to_string())
                    .send();
                let reply = async {
                    let resp = tokio::time::timeout(CHILD_TIMEOUT, sent).await.ok()?.ok()?;
                    let bytes = tokio::time::timeout(CHILD_TIMEOUT, resp.bytes())
                        .await
                        .ok()?
                        .ok()?;
                    let v: Value = serde_json::from_slice(&bytes).ok()?;
                    (v.get("ok").and_then(Value::as_bool) == Some(true)).then_some(v)
                }
                .await;
                let num = |ptr: &str| {
                    reply
                        .as_ref()
                        .and_then(|v| v.pointer(ptr))
                        .and_then(Value::as_u64)
                        .map(|n| n.min(u32::MAX as u64) as u32)
                };
                AccountLoad {
                    username: a.username,
                    running: true,
                    terminals: num("/data/open"),
                    claude_processes: num("/data/claude_procs"),
                }
            }
        });
    Some(futures_util::future::join_all(calls).await)
}
