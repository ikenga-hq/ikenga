//! WP-5 through the whole broker router: an RPC from a share member goes
//! cookie -> narrowing -> R-3 -> `approvals::gate` -> the owner's child.
//!
//! The launcher serves a recording fake child per principal (so the tests see
//! exactly which requests reached the OWNER's child, with which body), and a
//! fake sender listens on a temp Unix socket. The narrower is a stand-in for
//! `access::t1::AccessNarrower` that yields the broker's decision for a share
//! (role, owner); what is under test is everything after it. The unit tests
//! in `approvals.rs` cover the gate's cases one by one.

use axum::http::{HeaderName, HeaderValue};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

use super::*;
use crate::access::caps::{CapSet, Role, Tier};
use crate::access::ctx::{AccessCtx, RequestMeta, Via};
use crate::access::t1::BrokerCtx;
use crate::access::ShareCtx;
use crate::server::broker::approvals::ApprovalsConfig;
use crate::server::broker::proxy::{Narrower, Narrowing, Refusal, CAPS_HEADER};

/// One request a child saw: `(its principal, headers, parsed body)`.
type ChildSeen = Arc<Mutex<Vec<(PrincipalId, HeaderMap, Value)>>>;

struct RecordingLauncher {
    seen: ChildSeen,
}

impl ChildLauncher for RecordingLauncher {
    fn launch<'a>(
        &'a self,
        principal: &'a Principal,
        _token: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<LaunchedChild>> {
        let id = principal.id;
        let seen = self.seen.clone();
        Box::pin(async move {
            let app = Router::new().route(
                "/api/rpc",
                post(move |req: Request<Body>| {
                    let seen = seen.clone();
                    async move {
                        let (parts, body) = req.into_parts();
                        let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
                        let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                        seen.lock().unwrap().push((id, parts.headers, v.clone()));
                        if v["cmd"] == "pa_actions_list" {
                            // The owner's drafts. The agent that owns them also
                            // forges an approval on the sent one.
                            return Json(json!({ "ok": true, "data": [
                                { "id": DRAFT, "status": "awaiting", "channel": "smtp" },
                                { "id": "other", "status": "edited", "channel": "smtp" },
                                { "id": "done", "status": "sent", "channel": "smtp",
                                  "ledgerApproval": { "approver": { "principalId": "x",
                                      "username": "chinedum" }, "at": "t" } },
                            ] }));
                        }
                        Json(json!({ "ok": true, "data": { "cmd": v["cmd"] } }))
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let addr = listener.local_addr()?;
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            Ok(LaunchedChild {
                addr,
                process: Box::new(FakeProcess::default()),
            })
        })
    }
}

/// Every viewer is a member of `owner`'s project "ruby-jobs" with `role`.
pub(super) struct ShareOf {
    pub(super) owner: Principal,
    pub(super) role: Role,
}

impl Narrower for ShareOf {
    fn narrow<'a>(
        &'a self,
        ctx: &'a crate::server::auth::PrincipalCtx,
        _req: &'a axum::http::request::Parts,
    ) -> BoxFuture<'a, Result<Narrowing, Refusal>> {
        Box::pin(async move {
            let share = ShareCtx {
                project_key: format!("{}/ruby-jobs", self.owner.id),
                project_id: "ruby-jobs".into(),
                member_principal_id: Some(ctx.principal.id.to_string()),
                member_device_id: None,
                role: Some(self.role),
                artifact_path: None,
                owner_approval: false,
            };
            let mut headers = vec![(
                HeaderName::from_static(CAPS_HEADER),
                HeaderValue::from_str(&CapSet::ALL.to_header()).unwrap(),
            )];
            for (n, v) in crate::access::share::to_child_headers(&share) {
                headers.push((
                    HeaderName::from_static(n),
                    HeaderValue::from_str(&v).unwrap(),
                ));
            }
            let b = BrokerCtx {
                access: AccessCtx {
                    principal_id: ctx.principal.id,
                    via: Via::Session {
                        session_id: "s".into(),
                    },
                    device_id: None,
                    tier: Tier::Full,
                    share: Some(share),
                    share_headers: false,
                    caps: CapSet::ALL,
                    admin_strength: true,
                    is_admin: false,
                    meta: RequestMeta::default(),
                },
                owner: Some(self.owner.clone()),
                socket: 1,
            };
            Ok(Narrowing {
                headers,
                target: Some(self.owner.clone()),
                snapshot: Some(Arc::new(b)),
            })
        })
    }
}

const BASELINE_ROW: &str = include_str!("../testdata/approvals-contract-vectors.json");
const RENDERED: &str = "• **Send-time smtpFrom:** ruby@royalti.io\n\n**Body:**\n> Hi";

/// A sender that holds the contract's baseline row; records every request.
async fn fake_sender(
    dir: &std::path::Path,
    approve_reply: &'static str,
) -> (std::path::PathBuf, Arc<Mutex<Vec<Value>>>, Value) {
    let doc: Value = serde_json::from_str(BASELINE_ROW).unwrap();
    let v0 = doc["vectors"][0].clone();
    let hash = v0["sha256"].as_str().unwrap().to_string();
    let path = dir.join("approvals.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (log, hash) = (log.clone(), hash.clone());
            tokio::spawn(async move {
                let (r, mut w) = stream.into_split();
                let mut line = String::new();
                if BufReader::new(r).read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                let req: Value = serde_json::from_str(line.trim()).unwrap();
                let reply = match req["action"].as_str() {
                    Some("get_draft") => json!({
                        "ok": true, "draftId": req["draftId"], "status": "awaiting",
                        "payloadHash": hash, "renderedText": RENDERED,
                    })
                    .to_string(),
                    Some("approve") => approve_reply.to_string(),
                    // Contract r3: the ledger holds an approval for DRAFT only.
                    Some("approval_status") => json!({
                        "ok": true,
                        "approvals": { DRAFT: {
                            "approver": { "principalId": "p-onye", "username": "onyedikachi" },
                            "source": "mattermost", "at": "2026-10-24T10:00:00Z",
                            "consumed": false,
                        } },
                    })
                    .to_string(),
                    _ => json!({"ok": false, "error": "unknown_action"}).to_string(),
                };
                log.lock().unwrap().push(req);
                let _ = w.write_all(format!("{reply}\n").as_bytes()).await;
            });
        }
    });
    (path, seen, v0)
}

struct Rig {
    _tmp: tempfile::TempDir,
    pool: SqlitePool,
    app: Router,
    child_seen: ChildSeen,
    owner: Principal,
    row: Value,
}

async fn rig(role: Role, socket: Option<&'static str>) -> (Rig, Arc<Mutex<Vec<Value>>>) {
    let (tmp, root) = test_support::temp_root();
    let pool = open_accounts(&root, Opener::Broker).await.unwrap();
    let child_seen: ChildSeen = Arc::new(Mutex::new(Vec::new()));
    let launcher = Arc::new(RecordingLauncher {
        seen: child_seen.clone(),
    });
    let verifier = Arc::new(
        tokio::task::spawn_blocking(LoginVerifier::new)
            .await
            .unwrap(),
    );
    let owner = crate::server::broker::children::tests::principal();
    let mut state = BrokerState::new(pool.clone(), verifier, launcher).unwrap();
    state.hooks.narrower = Arc::new(ShareOf {
        owner: owner.clone(),
        role,
    });
    let sender_dir = tmp.path().join("run");
    std::fs::create_dir_all(&sender_dir).unwrap();
    let (log, row) = match socket {
        Some(reply) => {
            let (path, log, row) = fake_sender(&sender_dir, reply).await;
            state.approvals = ApprovalsConfig {
                socket: path,
                agent_uid_min: u32::MAX,
                ..ApprovalsConfig::default()
            };
            (log, row)
        }
        None => {
            // No sender at all.
            state.approvals = ApprovalsConfig::at(sender_dir.join("absent.sock"));
            let doc: Value = serde_json::from_str(BASELINE_ROW).unwrap();
            (Arc::new(Mutex::new(Vec::new())), doc["vectors"][0].clone())
        }
    };
    let store = backend::open_session_store(&root).await.unwrap();
    let app = router(
        Arc::new(state),
        store,
        &tmp.path().join("no-spa"),
        vec![],
        false,
        BrokerExtensions::default(),
    );
    (
        Rig {
            _tmp: tmp,
            pool,
            app,
            child_seen,
            owner,
            row,
        },
        log,
    )
}

fn commit_body(row: &Value, draft_id: &str) -> Value {
    json!({ "cmd": "pa_actions_commit", "args": {
        "draftId": draft_id,
        "seen": {
            "channel": row["row"]["channel"], "payloadJson": row["row"]["payload_json"],
            "editedJson": row["row"]["edited_json"], "scheduledAt": row["row"]["scheduled_at"],
        },
    }})
}

const OK: &str = r#"{"ok":true,"draftId":"ruby-reply:em-1001","source":"shell"}"#;
const DRAFT: &str = "ruby-reply:em-1001";

fn owner_requests(r: &Rig) -> Vec<(HeaderMap, Value)> {
    r.child_seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(id, _, _)| *id == r.owner.id)
        .map(|(_, h, b)| (h.clone(), b.clone()))
        .collect()
}

/// What reached the owner's child other than the broker's own reads of the
/// owner's drafts (`pa_actions_list`: before a by-id command it checks the
/// row is one this viewer may act on, `share_outreach`). Those change nothing.
fn owner_writes(r: &Rig) -> Vec<(HeaderMap, Value)> {
    owner_requests(r)
        .into_iter()
        .filter(|(_, b)| b["cmd"] != "pa_actions_list")
        .collect()
}

#[tokio::test]
async fn an_operators_commit_is_approved_by_the_broker_as_them_and_never_reaches_the_owners_child()
{
    let (r, log) = rig(Role::Operator, Some(OK)).await;
    let ada = insert_account(&r.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&r.app, "ada").await;

    let (status, body) = rpc(&r.app, Some(&cookie), commit_body(&r.row, DRAFT)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["data"]["draftId"], DRAFT, "{body}");
    assert_eq!(body["data"]["approved"], true, "{body}");

    // The ledger approval was written, as Ada (not the owner).
    let reqs = log.lock().unwrap().clone();
    let actions: Vec<_> = reqs.iter().map(|q| q["action"].as_str().unwrap()).collect();
    assert_eq!(actions, ["get_draft", "approve"]);
    assert_eq!(reqs[1]["source"], "shell");
    assert_eq!(reqs[1]["approver"]["id"], ada.to_string());
    assert_eq!(reqs[1]["approver"]["username"], "ada");
    assert_ne!(reqs[1]["approver"]["id"], r.owner.id.to_string());

    // The owner's child (the agent's daemon) takes no part: the sender sends
    // the draft on the ledger approval (contract r2).
    let got = owner_writes(&r);
    assert!(got.is_empty(), "{got:?}");
}

#[tokio::test]
async fn a_reviewers_commit_never_reaches_the_sender_or_the_owners_child() {
    let (r, log) = rig(Role::Reviewer, Some(OK)).await;
    insert_account(&r.pool, "onyedikachi", 20_002, false).await;
    let cookie = login_cookie(&r.app, "onyedikachi").await;
    for cmd in [
        "pa_actions_commit",
        "pa_actions_update",
        "pa_actions_reject",
        "pa_actions_list",
    ] {
        let mut body = commit_body(&r.row, DRAFT);
        body["cmd"] = json!(cmd);
        let (status, resp) = rpc(&r.app, Some(&cookie), body).await;
        assert_eq!(status, StatusCode::OK, "{cmd}: {resp}");
        assert_eq!(resp["ok"], false, "{cmd}: {resp}");
        assert!(
            resp["error"].as_str().unwrap().starts_with("forbidden: "),
            "{cmd}: {resp}"
        );
    }
    assert!(
        log.lock().unwrap().is_empty(),
        "the ledger socket was never touched"
    );
    assert!(
        owner_requests(&r).is_empty(),
        "the owner's child saw nothing"
    );
}

#[tokio::test]
async fn with_no_sender_socket_an_operators_commit_fails_closed() {
    let (r, _) = rig(Role::Operator, None).await;
    insert_account(&r.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&r.app, "ada").await;
    let (status, body) = rpc(&r.app, Some(&cookie), commit_body(&r.row, DRAFT)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], false, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .starts_with("approval_unavailable: "),
        "{body}"
    );
    assert!(
        owner_writes(&r).is_empty(),
        "no commit without a ledger approval"
    );
}

#[tokio::test]
async fn a_sender_refusal_is_shown_and_the_commit_is_not_forwarded() {
    let (r, log) = rig(
        Role::Operator,
        Some(r#"{"ok":false,"error":"unauthorized_approver"}"#),
    )
    .await;
    insert_account(&r.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&r.app, "ada").await;
    let (_, body) = rpc(&r.app, Some(&cookie), commit_body(&r.row, DRAFT)).await;
    assert_eq!(body["ok"], false, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("not on the box's list"),
        "{body}"
    );
    assert_eq!(body["error_data"]["code"], "approval_refused");
    assert_eq!(
        log.lock().unwrap().len(),
        2,
        "get_draft then the refused approve"
    );
    assert!(owner_writes(&r).is_empty());
}

#[tokio::test]
async fn an_operator_lists_updates_and_rejects_through_the_owners_child_without_touching_the_ledger(
) {
    let (r, log) = rig(Role::Operator, Some(OK)).await;
    insert_account(&r.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&r.app, "ada").await;
    // The outbox redesign's whole patch (G-CONTRACT) and a reject reason reach
    // the owner's child exactly as sent: the merge and the reason are the
    // child's (`shared::pa_actions`), the broker adds and drops nothing.
    let update_args = json!({"draftId": DRAFT, "patch": {
        "body": "x", "subjectB": "B", "preheader": "p", "firstComment": "c",
        "hashtags": ["#a"], "media": {"kind": "carousel", "imageUrls": ["https://x/1.png"]},
        "scheduledIso": "2026-11-03T09:00:00+01:00"}});
    let reject_args = json!({"draftId": DRAFT, "reason": "Wrong tone"});
    for (cmd, args) in [
        ("pa_actions_list", json!({"status": null})),
        ("pa_actions_update", update_args.clone()),
        ("pa_actions_reject", reject_args.clone()),
    ] {
        let (_, body) = rpc(&r.app, Some(&cookie), json!({"cmd": cmd, "args": args})).await;
        assert_eq!(body["ok"], true, "{cmd}: {body}");
    }
    let got = owner_requests(&r);
    let forwarded = |cmd: &str| {
        got.iter()
            .find(|(_, b)| b["cmd"] == cmd)
            .map(|(_, b)| b["args"].clone())
    };
    assert_eq!(forwarded("pa_actions_update"), Some(update_args));
    assert_eq!(forwarded("pa_actions_reject"), Some(reject_args));
    let cmds: Vec<_> = got
        .iter()
        .map(|(_, b)| b["cmd"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        cmds,
        // Ada is not shown every outreach row, so the broker reads the owner's
        // drafts before each by-id command to check the row is hers to act on.
        [
            "pa_actions_list",
            "pa_actions_list",
            "pa_actions_update",
            "pa_actions_list",
            "pa_actions_reject"
        ]
    );
    // Listing may ask the ledger who approved what (`approval_status`, read
    // only); it never writes an approval.
    let actions: Vec<String> = log
        .lock()
        .unwrap()
        .iter()
        .map(|q| q["action"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(actions, ["approval_status"]);
}

#[tokio::test]
async fn an_operators_list_marks_the_drafts_the_ledger_approved_and_nothing_else() {
    let (r, log) = rig(Role::Operator, Some(OK)).await;
    insert_account(&r.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&r.app, "ada").await;
    let (status, body) = rpc(
        &r.app,
        Some(&cookie),
        json!({"cmd": "pa_actions_list", "args": {"status": null}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true, "{body}");
    let rows = body["data"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "{body}");
    assert_eq!(rows[0]["id"], DRAFT);
    assert_eq!(
        rows[0]["ledgerApproval"]["approver"]["username"],
        "onyedikachi"
    );
    assert_eq!(rows[0]["ledgerApproval"]["at"], "2026-10-24T10:00:00Z");
    assert!(
        rows[1].get("ledgerApproval").is_none(),
        "no approval, no mark"
    );
    // The child's own forged mark on a sent row is stripped.
    assert!(rows[2].get("ledgerApproval").is_none(), "{body}");

    // Only awaiting/edited ids were asked about, and only to read.
    let reqs = log.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0]["action"], "approval_status");
    assert_eq!(reqs[0]["draftIds"], json!([DRAFT, "other"]));
    // It was the owner's child that listed.
    let got = owner_requests(&r);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].1["cmd"], "pa_actions_list");
}

#[tokio::test]
async fn with_no_sender_socket_the_list_still_arrives_unannotated() {
    let (r, _) = rig(Role::Operator, None).await;
    insert_account(&r.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&r.app, "ada").await;
    let (status, body) = rpc(
        &r.app,
        Some(&cookie),
        json!({"cmd": "pa_actions_list", "args": {"status": null}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true, "{body}");
    let rows = body["data"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "{body}");
    assert!(
        rows.iter().all(|r| r.get("ledgerApproval").is_none()),
        "{body}"
    );
}

/// A principal's own workspace (no share) is untouched: no socket, no gate.
#[tokio::test]
async fn outside_a_share_nothing_changes() {
    struct Own;
    impl Narrower for Own {
        fn narrow<'a>(
            &'a self,
            _ctx: &'a crate::server::auth::PrincipalCtx,
            _req: &'a axum::http::request::Parts,
        ) -> BoxFuture<'a, Result<Narrowing, Refusal>> {
            Box::pin(async { Ok(Narrowing::default()) })
        }
    }
    let (mut r, _) = rig(Role::Operator, None).await;
    // Rebuild the router with the no-share narrower.
    let (tmp, root) = test_support::temp_root();
    let pool = open_accounts(&root, Opener::Broker).await.unwrap();
    let launcher = Arc::new(RecordingLauncher {
        seen: r.child_seen.clone(),
    });
    let verifier = Arc::new(
        tokio::task::spawn_blocking(LoginVerifier::new)
            .await
            .unwrap(),
    );
    let mut state = BrokerState::new(pool.clone(), verifier, launcher).unwrap();
    state.hooks.narrower = Arc::new(Own);
    state.approvals = ApprovalsConfig::at(tmp.path().join("absent.sock"));
    let store = backend::open_session_store(&root).await.unwrap();
    r.app = router(
        Arc::new(state),
        store,
        &tmp.path().join("no-spa"),
        vec![],
        false,
        BrokerExtensions::default(),
    );
    let ada = insert_account(&pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&r.app, "ada").await;
    let (_, body) = rpc(
        &r.app,
        Some(&cookie),
        json!({"cmd": "pa_actions_commit", "args": {"draftId": "mine"}}),
    )
    .await;
    assert_eq!(body["ok"], true, "{body}");
    let seen = r.child_seen.lock().unwrap().clone();
    let (id, _, b) = seen.last().unwrap();
    assert_eq!(*id, ada, "her own child");
    assert_eq!(b["args"], json!({"draftId": "mine"}));
}
