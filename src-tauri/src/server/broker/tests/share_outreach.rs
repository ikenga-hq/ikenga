//! WP-D through the whole broker router: the per-person view of outreach drafts
//! (box plan `2026-10-25-box-outreach-sender`). Ada sees and acts on her own
//! outreach rows, Chinedum on every row, anyone else on none; a row a viewer may
//! not see cannot be listed, fetched, edited, approved or rejected by id, and
//! asking about it reads exactly like asking about an id that does not exist.
//!
//! The owner's child here is a small in-memory `pa_action_drafts` that keeps the
//! real statuses and the real refusal texts, and records every mutation that
//! reaches it, so "Ada could not change Chinedum's row" is checked against the
//! row, not only against the response. A fake sender listens on a temp Unix
//! socket (contract r3 `approval_status`, r5 `outreach_status`, `approve`).

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

use super::share_approve::ShareOf;
use super::*;
use crate::access::caps::Role;
use crate::server::broker::approvals::ApprovalsConfig;
use crate::server::shared::pa_actions as core;

const VECTORS: &str = include_str!("../testdata/approvals-contract-vectors.json");
const RENDERED: &str = "• **Send-time smtpFrom:** ruby@royalti.io\n\n**Body:**\n> Hi";
/// The contract's baseline vector id: the one draft the fake sender can hash.
const OWN_APPROVABLE: &str = "ruby-reply:em-1001";

const A_FIRST: &str = "outreach:a000000000000001";
const A_EDITED: &str = "outreach:a000000000000002";
const A_SENT: &str = "outreach:a000000000000003";
const C_FIRST: &str = "outreach:c000000000000001";
const C_SENT: &str = "outreach:c000000000000002";
const C_COMMITTED: &str = "outreach:c000000000000003";
const REPLY: &str = "ruby-reply:em-7";
const GHOST: &str = "outreach:ffffffffffffffff";

fn outreach_row(id: &str, assignee: &str, rank: u32, status: &str) -> Value {
    let payload = json!({
        "item": {
            "recipientEmail": "p@pub.example", "subject": "s", "body": "b",
            "outreach": { "assignee": assignee, "from_identity": assignee, "send_rank": rank,
                          "template": "A", "region": "uk-eu", "company": "Pub", "step": "first" },
        },
        "meta": { "agent": "Ruby" },
        "source": { "kind": "outreach_email" },
    });
    json!({ "id": id, "batchId": "outreach:2026-10-25", "actionId": "ruby/outreach-email",
            "status": status, "channel": "smtp", "payloadJson": payload.to_string() })
}

fn reply_row(id: &str) -> Value {
    json!({ "id": id, "batchId": "b", "actionId": "ruby/reply", "status": "awaiting",
            "channel": "smtp",
            "payloadJson": json!({"item": {"subject": "Re"}, "meta": {}}).to_string() })
}

/// The owner's drafts and what was done to them.
struct Db {
    rows: Mutex<Vec<Value>>,
    /// `(cmd, draft id)` for every update / reject / commit that reached the child.
    mutations: Mutex<Vec<(String, String)>>,
    /// Make `pa_actions_list` fail (the child cannot be read).
    broken: std::sync::atomic::AtomicBool,
}

fn seed() -> Vec<Value> {
    vec![
        outreach_row(C_FIRST, "chinedum", 1, "awaiting"),
        outreach_row(A_FIRST, "ada", 1, "awaiting"),
        outreach_row(A_EDITED, "ada", 2, "edited"),
        outreach_row(C_COMMITTED, "chinedum", 2, "committed"),
        reply_row(REPLY),
        outreach_row(A_SENT, "ada", 3, "sent"),
        outreach_row(C_SENT, "chinedum", 3, "sent"),
        outreach_row(OWN_APPROVABLE, "ada", 4, "awaiting"),
    ]
}

const ACTIVE: [&str; 4] = ["awaiting", "edited", "committed", "failed"];

struct OutreachLauncher {
    db: Arc<Db>,
    seen: ChildSeen,
}

type ChildSeen = Arc<Mutex<Vec<(PrincipalId, Value)>>>;

impl ChildLauncher for OutreachLauncher {
    fn launch<'a>(
        &'a self,
        principal: &'a Principal,
        _token: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<LaunchedChild>> {
        let (id, db, seen) = (principal.id, self.db.clone(), self.seen.clone());
        Box::pin(async move {
            let app = Router::new().route(
                "/api/rpc",
                post(move |req: Request<Body>| {
                    let (db, seen) = (db.clone(), seen.clone());
                    async move {
                        let body = axum::body::to_bytes(req.into_body(), usize::MAX)
                            .await
                            .unwrap();
                        let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                        seen.lock().unwrap().push((id, v.clone()));
                        let cmd = v["cmd"].as_str().unwrap_or("").to_string();
                        let args = &v["args"];
                        let draft = args["draftId"]
                            .as_str()
                            .or(args["draft_id"].as_str())
                            .unwrap_or("");
                        let mut rows = db.rows.lock().unwrap();
                        match cmd.as_str() {
                            "pa_actions_list" => {
                                if db.broken.load(std::sync::atomic::Ordering::SeqCst) {
                                    return Json(
                                        json!({"ok": false, "error": "pa_actions_list: boom"}),
                                    );
                                }
                                let out: Vec<Value> = rows
                                    .iter()
                                    .filter(|r| match args["status"].as_str() {
                                        Some(s) => r["status"] == s,
                                        None => ACTIVE.contains(&r["status"].as_str().unwrap()),
                                    })
                                    .cloned()
                                    .collect();
                                Json(json!({"ok": true, "data": out}))
                            }
                            "pa_actions_update" | "pa_actions_reject" | "pa_actions_commit" => {
                                // The real child checks `patch` before it touches the DB.
                                if cmd == "pa_actions_update" {
                                    if let Some(e) = core::patch_error(args.get("patch")) {
                                        return Json(json!({
                                            "ok": false,
                                            "error": format!("{cmd}: {e}"),
                                        }));
                                    }
                                }
                                let allowed: &[&str] = match cmd.as_str() {
                                    "pa_actions_update" => &["awaiting", "edited"],
                                    "pa_actions_reject" => &ACTIVE,
                                    _ => &["awaiting", "edited"],
                                };
                                let Some(row) = rows.iter_mut().find(|r| {
                                    r["id"] == draft
                                        && allowed.contains(&r["status"].as_str().unwrap())
                                }) else {
                                    let why = if cmd == "pa_actions_update" {
                                        core::update_refusal(draft)
                                    } else {
                                        core::reject_refusal(draft)
                                    };
                                    return Json(
                                        json!({"ok": false, "error": format!("{cmd}: {why}")}),
                                    );
                                };
                                db.mutations
                                    .lock()
                                    .unwrap()
                                    .push((cmd.clone(), draft.to_string()));
                                if cmd == "pa_actions_reject" {
                                    row["status"] = json!("rejected");
                                }
                                Json(json!({"ok": true, "data": null}))
                            }
                            _ => Json(json!({"ok": true, "data": {"cmd": cmd}})),
                        }
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

/// What the fake sender says about outreach status.
#[derive(Clone)]
struct SenderScript {
    pause: Value,
    rows: Value,
}

async fn fake_sender(
    dir: &std::path::Path,
    script: SenderScript,
) -> (std::path::PathBuf, Arc<Mutex<Vec<Value>>>) {
    let doc: Value = serde_json::from_str(VECTORS).unwrap();
    let hash = doc["vectors"][0]["sha256"].as_str().unwrap().to_string();
    let path = dir.join("approvals.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (log, hash, script) = (log.clone(), hash.clone(), script.clone());
            tokio::spawn(async move {
                let (r, mut w) = stream.into_split();
                let mut line = String::new();
                if BufReader::new(r).read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                let req: Value = serde_json::from_str(line.trim()).unwrap();
                let reply = match req["action"].as_str() {
                    Some("get_draft") if req["draftId"] == OWN_APPROVABLE => json!({
                        "ok": true, "draftId": OWN_APPROVABLE, "status": "awaiting",
                        "payloadHash": hash, "renderedText": RENDERED,
                    }),
                    Some("get_draft") => json!({"ok": false, "error": "draft_not_found"}),
                    Some("approve") => {
                        json!({"ok": true, "draftId": req["draftId"], "source": "shell"})
                    }
                    Some("approval_status") => json!({"ok": true, "approvals": {}}),
                    Some("outreach_status") => json!({"ok": true, "outreach": {
                        "pause": script.pause, "counts": {"today": {"ada": 1, "chinedum": 2}},
                        "rows": script.rows}}),
                    _ => json!({"ok": false, "error": "unknown_action"}),
                };
                log.lock().unwrap().push(req);
                let _ = w.write_all(format!("{reply}\n").as_bytes()).await;
            });
        }
    });
    (path, seen)
}

struct Rig {
    _tmp: tempfile::TempDir,
    pool: SqlitePool,
    app: Router,
    db: Arc<Db>,
    child_seen: ChildSeen,
    sender: Arc<Mutex<Vec<Value>>>,
}

async fn rig(role: Role, script: Option<SenderScript>) -> Rig {
    let (tmp, root) = test_support::temp_root();
    let pool = open_accounts(&root, Opener::Broker).await.unwrap();
    let db = Arc::new(Db {
        rows: Mutex::new(seed()),
        mutations: Mutex::default(),
        broken: Default::default(),
    });
    let child_seen: ChildSeen = Arc::default();
    let launcher = Arc::new(OutreachLauncher {
        db: db.clone(),
        seen: child_seen.clone(),
    });
    let verifier = Arc::new(
        tokio::task::spawn_blocking(LoginVerifier::new)
            .await
            .unwrap(),
    );
    let owner = crate::server::broker::children::tests::principal();
    let mut state = BrokerState::new(pool.clone(), verifier, launcher).unwrap();
    state.hooks.narrower = Arc::new(ShareOf { owner, role });
    let run = tmp.path().join("run");
    std::fs::create_dir_all(&run).unwrap();
    let sender = match script {
        Some(script) => {
            let (path, log) = fake_sender(&run, script).await;
            state.approvals = ApprovalsConfig {
                socket: path,
                agent_uid_min: u32::MAX,
                ..ApprovalsConfig::default()
            };
            log
        }
        None => {
            state.approvals = ApprovalsConfig::at(run.join("absent.sock"));
            Arc::default()
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
    Rig {
        _tmp: tmp,
        pool,
        app,
        db,
        child_seen,
        sender,
    }
}

fn quiet() -> SenderScript {
    SenderScript {
        pause: json!(null),
        rows: json!({}),
    }
}

async fn as_user(r: &Rig, name: &str, uid: u32) -> String {
    insert_account(&r.pool, name, uid, false).await;
    login_cookie(&r.app, name).await
}

async fn list(r: &Rig, cookie: &str) -> Value {
    let (status, body) = rpc(
        &r.app,
        Some(cookie),
        json!({"cmd": "pa_actions_list", "args": {"status": null}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true, "{body}");
    body
}

fn ids(body: &Value) -> Vec<String> {
    body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
}

/// What reached the owner's child, as `(cmd, args)`, ignoring the broker's own
/// lookups (`pa_actions_list`).
fn child_actions(r: &Rig) -> Vec<(String, Value)> {
    r.child_seen
        .lock()
        .unwrap()
        .iter()
        .map(|(_, v)| (v["cmd"].as_str().unwrap().to_string(), v["args"].clone()))
        .filter(|(c, _)| c != "pa_actions_list")
        .collect()
}

fn commit_of(id: &str) -> Value {
    let doc: Value = serde_json::from_str(VECTORS).unwrap();
    let row = &doc["vectors"][0]["row"];
    json!({"cmd": "pa_actions_commit", "args": {"draftId": id, "seen": {
        "channel": row["channel"], "payloadJson": row["payload_json"],
        "editedJson": row["edited_json"], "scheduledAt": row["scheduled_at"] }}})
}

fn with_id(cmd: &str, id: &str) -> Value {
    match cmd {
        "pa_actions_commit" => commit_of(id),
        "pa_actions_update" => json!({"cmd": cmd, "args": {"draftId": id, "patch": {"body": "x"}}}),
        _ => json!({"cmd": cmd, "args": {"draftId": id}}),
    }
}

#[tokio::test]
async fn ada_is_listed_her_own_outreach_rows_and_the_plain_drafts_only() {
    let r = rig(Role::Operator, Some(quiet())).await;
    let ada = as_user(&r, "ada", 20_001).await;
    let body = list(&r, &ada).await;
    // The child's order is kept (the Outbox ranks); Chinedum's rows are gone.
    assert_eq!(ids(&body), [A_FIRST, A_EDITED, REPLY, OWN_APPROVABLE]);
    // Nothing of Chinedum's reaches the page, not even as a substring.
    let text = body.to_string();
    assert!(!text.contains("chinedum"), "{text}");
    for id in [C_FIRST, C_COMMITTED] {
        assert!(!text.contains(id));
    }
    // Her list is the broker's decision from her login, not her request: a body that asks for more gets the same.
    let (_, wide) = rpc(
        &r.app,
        Some(&ada),
        json!({"cmd": "pa_actions_list",
        "args": {"status": null, "assignee": "chinedum", "all": true}}),
    )
    .await;
    assert_eq!(ids(&wide), ids(&body));
    // Sent rows are hers or nothing too.
    let (_, sent) = rpc(
        &r.app,
        Some(&ada),
        json!({"cmd": "pa_actions_list", "args": {"status": "sent"}}),
    )
    .await;
    assert_eq!(ids(&sent), [A_SENT]);
}

#[tokio::test]
async fn chinedum_is_listed_every_row() {
    let r = rig(Role::Operator, Some(quiet())).await;
    let me = as_user(&r, "chinedum", 20_002).await;
    let body = list(&r, &me).await;
    assert_eq!(
        ids(&body),
        [
            C_FIRST,
            A_FIRST,
            A_EDITED,
            C_COMMITTED,
            REPLY,
            OWN_APPROVABLE
        ]
    );
    let (_, sent) = rpc(
        &r.app,
        Some(&me),
        json!({"cmd": "pa_actions_list", "args": {"status": "sent"}}),
    )
    .await;
    assert_eq!(ids(&sent), [A_SENT, C_SENT]);
}

#[tokio::test]
async fn any_other_operator_is_shown_no_outreach_row() {
    let r = rig(Role::Operator, Some(quiet())).await;
    let bob = as_user(&r, "bob", 20_003).await;
    assert_eq!(ids(&list(&r, &bob).await), [REPLY]);
}

#[tokio::test]
async fn ada_cannot_edit_approve_or_reject_a_chinedum_row_and_it_reads_as_a_missing_id() {
    let r = rig(Role::Operator, Some(quiet())).await;
    let ada = as_user(&r, "ada", 20_001).await;
    // Every status of Chinedum's: awaiting, committed, sent.
    for hidden in [C_FIRST, C_COMMITTED, C_SENT] {
        for cmd in [
            "pa_actions_update",
            "pa_actions_reject",
            "pa_actions_commit",
        ] {
            let (s1, got) = rpc(&r.app, Some(&ada), with_id(cmd, hidden)).await;
            let (s2, want) = rpc(&r.app, Some(&ada), with_id(cmd, GHOST)).await;
            assert_eq!((s1, s2), (StatusCode::OK, StatusCode::OK));
            assert_eq!(got["ok"], false, "{cmd} {hidden}: {got}");
            // The same answer as for an id that exists nowhere, up to the id echoed back.
            assert_eq!(
                got.to_string().replace(hidden, "<id>"),
                want.to_string().replace(GHOST, "<id>"),
                "{cmd} {hidden}"
            );
        }
    }
    // Nothing was changed, forwarded or approved.
    assert!(
        r.db.mutations.lock().unwrap().is_empty(),
        "{:?}",
        r.db.mutations
    );
    // Only the probes for the id that exists nowhere were forwarded.
    assert!(
        child_actions(&r).iter().all(|(_, a)| a["draftId"] == GHOST),
        "{:?}",
        child_actions(&r)
    );
    assert!(
        r.sender
            .lock()
            .unwrap()
            .iter()
            .all(|q| q["action"] != "approve"),
        "no approval was written"
    );
    // The sender is asked only to read a hidden id (so a down sender answers the
    // same as for a missing id), never to approve it.
    for q in r.sender.lock().unwrap().iter() {
        let asked = q["draftId"].as_str().unwrap_or("");
        if [C_FIRST, C_COMMITTED, C_SENT].contains(&asked) {
            assert_eq!(q["action"], "get_draft", "{q}");
        }
    }
    // And the row is exactly as it was.
    let rows = r.db.rows.lock().unwrap().clone();
    assert_eq!(
        rows.iter().find(|x| x["id"] == C_FIRST).unwrap()["status"],
        "awaiting"
    );
    assert_eq!(
        rows.iter().find(|x| x["id"] == C_COMMITTED).unwrap()["status"],
        "committed"
    );
}

#[tokio::test]
async fn the_ledger_is_not_even_asked_about_a_row_the_viewer_may_not_see() {
    let r = rig(Role::Operator, Some(quiet())).await;
    let ada = as_user(&r, "ada", 20_001).await;
    let _ = list(&r, &ada).await;
    let named: Vec<String> = r
        .sender
        .lock()
        .unwrap()
        .iter()
        .flat_map(|q| q["draftIds"].as_array().cloned().unwrap_or_default())
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(!named.is_empty());
    assert!(
        !named.iter().any(|i| i == C_FIRST || i == C_COMMITTED),
        "{named:?}"
    );
}

#[tokio::test]
async fn ada_edits_rejects_and_approves_her_own_rows() {
    let r = rig(Role::Operator, Some(quiet())).await;
    let ada = as_user(&r, "ada", 20_001).await;
    let (_, b) = rpc(&r.app, Some(&ada), with_id("pa_actions_update", A_FIRST)).await;
    assert_eq!(b["ok"], true, "{b}");
    let (_, b) = rpc(&r.app, Some(&ada), with_id("pa_actions_reject", A_EDITED)).await;
    assert_eq!(b["ok"], true, "{b}");
    let (_, b) = rpc(&r.app, Some(&ada), commit_of(OWN_APPROVABLE)).await;
    assert_eq!(b["ok"], true, "{b}");
    assert_eq!(b["data"]["approved"], true);
    assert_eq!(
        *r.db.mutations.lock().unwrap(),
        [
            ("pa_actions_update".to_string(), A_FIRST.to_string()),
            ("pa_actions_reject".to_string(), A_EDITED.to_string())
        ]
    );
    let approves: Vec<Value> = r
        .sender
        .lock()
        .unwrap()
        .iter()
        .filter(|q| q["action"] == "approve")
        .cloned()
        .collect();
    assert_eq!(approves.len(), 1);
    assert_eq!(approves[0]["approver"]["username"], "ada");
}

#[tokio::test]
async fn chinedum_acts_on_any_row_and_a_plain_draft_is_open_to_both() {
    let r = rig(Role::Operator, Some(quiet())).await;
    let me = as_user(&r, "chinedum", 20_002).await;
    for id in [C_FIRST, A_FIRST] {
        let (_, b) = rpc(&r.app, Some(&me), with_id("pa_actions_reject", id)).await;
        assert_eq!(b["ok"], true, "{id}: {b}");
    }
    let ada = as_user(&r, "ada", 20_001).await;
    let (_, b) = rpc(&r.app, Some(&ada), with_id("pa_actions_update", REPLY)).await;
    assert_eq!(b["ok"], true, "{b}");
}

#[tokio::test]
async fn another_operator_cannot_act_on_any_outreach_row_by_id() {
    let r = rig(Role::Operator, Some(quiet())).await;
    let bob = as_user(&r, "bob", 20_003).await;
    for id in [A_FIRST, C_FIRST] {
        for cmd in [
            "pa_actions_update",
            "pa_actions_reject",
            "pa_actions_commit",
        ] {
            let (_, b) = rpc(&r.app, Some(&bob), with_id(cmd, id)).await;
            assert_eq!(b["ok"], false, "{cmd} {id}: {b}");
        }
    }
    assert!(r.db.mutations.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_id_smuggled_in_a_second_key_does_not_change_which_row_is_acted_on() {
    let r = rig(Role::Operator, Some(quiet())).await;
    let ada = as_user(&r, "ada", 20_001).await;
    // draftId is the one the child reads; naming Chinedum's there is refused...
    let (_, b) = rpc(
        &r.app,
        Some(&ada),
        json!({"cmd": "pa_actions_reject",
        "args": {"draftId": C_FIRST, "draft_id": A_FIRST}}),
    )
    .await;
    assert_eq!(b["ok"], false, "{b}");
    // ...and a non-string first key cannot make the check read the other key.
    let (_, b) = rpc(
        &r.app,
        Some(&ada),
        json!({"cmd": "pa_actions_reject",
        "args": {"draftId": 7, "draft_id": A_FIRST}}),
    )
    .await;
    assert_eq!(b["ok"], false, "{b}");
    assert!(r.db.mutations.lock().unwrap().is_empty());
}

#[tokio::test]
async fn when_the_owners_drafts_cannot_be_read_nothing_is_forwarded() {
    let r = rig(Role::Operator, Some(quiet())).await;
    let ada = as_user(&r, "ada", 20_001).await;
    r.db.broken.store(true, std::sync::atomic::Ordering::SeqCst);
    for cmd in [
        "pa_actions_update",
        "pa_actions_reject",
        "pa_actions_commit",
    ] {
        let (_, b) = rpc(&r.app, Some(&ada), with_id(cmd, A_FIRST)).await;
        assert_eq!(b["ok"], false, "{cmd}: {b}");
    }
    assert!(child_actions(&r).is_empty());
    assert!(r
        .sender
        .lock()
        .unwrap()
        .iter()
        .all(|q| q["action"] != "approve"));
}

#[tokio::test]
async fn rows_are_labelled_from_the_sender_and_the_pause_banner_rides_on_her_rows() {
    let script = SenderScript {
        pause: json!({"id": 2, "at": "2026-10-26T09:00:00Z", "sentCount": 30, "threshold": 30, "cardPostId": null}),
        rows: json!({
            A_FIRST: {"state": "queued_for_tomorrow", "reason": "queued_for_tomorrow", "updatedAt": "2026-10-26T08:00:00Z"},
            A_EDITED: {"state": "suppressed", "reason": "suppressed:domain"},
            C_FIRST: {"state": "paused"},
        }),
    };
    let r = rig(Role::Operator, Some(script)).await;
    let ada = as_user(&r, "ada", 20_001).await;
    let body = list(&r, &ada).await;
    let by = |id: &str| {
        body["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(
        by(A_FIRST)["outreachStatus"]["state"],
        "queued_for_tomorrow"
    );
    assert_eq!(by(A_EDITED)["outreachStatus"]["state"], "suppressed");
    assert_eq!(by(A_FIRST)["outreachPause"]["sentCount"], 30);
    assert!(by(REPLY).get("outreachStatus").is_none() && by(REPLY).get("outreachPause").is_none());
    // The other sender's counts and Chinedum's row notes do not reach her.
    let text = body.to_string();
    assert!(!text.contains("today") && !text.contains(C_FIRST), "{text}");
}

#[tokio::test]
async fn with_no_sender_the_ranked_rows_still_arrive_unlabelled() {
    let r = rig(Role::Operator, None).await;
    let ada = as_user(&r, "ada", 20_001).await;
    let body = list(&r, &ada).await;
    assert_eq!(ids(&body), [A_FIRST, A_EDITED, REPLY, OWN_APPROVABLE]);
    assert!(body["data"]
        .as_array()
        .unwrap()
        .iter()
        .all(|x| x.get("outreachStatus").is_none()
            && x.get("outreachPause").is_none()
            && x.get("ledgerApproval").is_none()));
}

#[tokio::test]
async fn a_reviewer_is_shown_nothing_at_all() {
    let r = rig(Role::Reviewer, Some(quiet())).await;
    let ada = as_user(&r, "ada", 20_001).await;
    let (_, b) = rpc(
        &r.app,
        Some(&ada),
        json!({"cmd": "pa_actions_list", "args": {"status": null}}),
    )
    .await;
    assert_eq!(b["ok"], false, "{b}");
    assert!(b["error"].as_str().unwrap().starts_with("forbidden: "));
}

/// A hidden id and an id that exists nowhere must answer identically whatever is
/// wrong with the request, or the difference tells the viewer the row is there.
async fn same_answer(r: &Rig, cookie: &str, hidden: Value, ghost: Value, hid: &str, tag: &str) {
    let (s1, got) = rpc(&r.app, Some(cookie), hidden).await;
    let (s2, want) = rpc(&r.app, Some(cookie), ghost).await;
    assert_eq!(s1, s2, "{tag}");
    assert_eq!(
        got.to_string().replace(hid, "<id>"),
        want.to_string().replace(GHOST, "<id>"),
        "{tag}: a hidden row is told apart from a missing one"
    );
}

#[tokio::test]
async fn a_commit_without_a_valid_seen_does_not_tell_a_hidden_row_from_a_missing_one() {
    let r = rig(Role::Operator, Some(quiet())).await;
    let ada = as_user(&r, "ada", 20_001).await;
    for bad in [None, Some(json!({})), Some(json!("x")), Some(json!(7))] {
        for hid in [C_FIRST, C_COMMITTED, C_SENT, A_FIRST] {
            let mk = |id: &str| {
                let mut args = json!({ "draftId": id });
                if let Some(b) = &bad {
                    args["seen"] = b.clone();
                }
                json!({"cmd": "pa_actions_commit", "args": args})
            };
            same_answer(
                &r,
                &ada,
                mk(hid),
                mk(GHOST),
                hid,
                &format!("seen {bad:?} {hid}"),
            )
            .await;
        }
    }
    assert!(r.db.mutations.lock().unwrap().is_empty());
    assert!(r
        .sender
        .lock()
        .unwrap()
        .iter()
        .all(|q| q["action"] != "approve"));
}

#[tokio::test]
async fn an_update_without_a_patch_does_not_tell_a_hidden_row_from_a_missing_one() {
    let r = rig(Role::Operator, Some(quiet())).await;
    let ada = as_user(&r, "ada", 20_001).await;
    for hid in [C_FIRST, C_COMMITTED, C_SENT, A_FIRST] {
        let mk = |id: &str| json!({"cmd": "pa_actions_update", "args": {"draftId": id}});
        same_answer(
            &r,
            &ada,
            mk(hid),
            mk(GHOST),
            hid,
            &format!("no patch {hid}"),
        )
        .await;
    }
    // A patch that is not an object (JSON null included), or whose `scheduledIso`
    // is neither a string nor null, is refused before any lookup, the same for a
    // hidden id as for a missing one.
    for bad in [
        json!(null),
        json!("x"),
        json!([1]),
        json!({"scheduledIso": 7}),
    ] {
        let mk =
            |id: &str| json!({"cmd": "pa_actions_update", "args": {"draftId": id, "patch": bad}});
        same_answer(
            &r,
            &ada,
            mk(C_FIRST),
            mk(GHOST),
            C_FIRST,
            &format!("patch {bad}"),
        )
        .await;
    }
    assert!(r.db.mutations.lock().unwrap().is_empty());
}

#[tokio::test]
async fn with_the_sender_down_a_hidden_commit_answers_as_a_missing_id_does() {
    let r = rig(Role::Operator, None).await;
    let ada = as_user(&r, "ada", 20_001).await;
    for hid in [C_FIRST, C_SENT] {
        same_answer(
            &r,
            &ada,
            commit_of(hid),
            commit_of(GHOST),
            hid,
            &format!("down {hid}"),
        )
        .await;
    }
}
