//! WP-K relay tests: identity from the session, the shell's first layer, the
//! no-value guarantee (a canary), and the sender-error mapping. A fake sender
//! speaks newline JSON on a Unix socket, like `approvals.rs`'s tests.

use super::*;
use std::path::PathBuf;
use std::sync::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

const CANARY: &str = "canary-S3cr3t-9f2e71";

fn me() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

type Handler = Arc<dyn Fn(&Value) -> String + Send + Sync>;

struct FakeSender {
    _dir: tempfile::TempDir,
    path: PathBuf,
    seen: Arc<Mutex<Vec<Value>>>,
}

impl FakeSender {
    async fn start(handler: Handler) -> FakeSender {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approvals.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (log, handler) = (log.clone(), handler.clone());
                tokio::spawn(async move {
                    let (r, mut w) = stream.into_split();
                    let mut line = String::new();
                    if BufReader::new(r).read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let req: Value = serde_json::from_str(line.trim()).unwrap();
                    let reply = handler(&req);
                    log.lock().unwrap().push(req);
                    let _ = w.write_all(format!("{reply}\n").as_bytes()).await;
                });
            }
        });
        FakeSender {
            _dir: dir,
            path,
            seen,
        }
    }

    fn cfg(&self) -> ApprovalsConfig {
        ApprovalsConfig {
            socket: self.path.clone(),
            agent_uid_min: u32::MAX,
            sender_uid: Some(me()),
            ..ApprovalsConfig::default()
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }

    fn actions(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|r| r["action"].as_str().unwrap().to_string())
            .collect()
    }
}

fn member() -> Caller {
    Caller {
        principal_id: "0192-ada".into(),
        username: "ada".into(),
        is_admin: false,
    }
}

fn admin() -> Caller {
    Caller {
        principal_id: "0192-chi".into(),
        username: "chinedum".into(),
        is_admin: true,
    }
}

/// A sender that knows `person` for the caller and accepts everything else.
fn sender(person: &'static str, admin: bool) -> Handler {
    Arc::new(move |req: &Value| {
        match req["action"].as_str().unwrap() {
        "vault.status" => {
            json!({"ok": true, "person": person, "admin": admin, "groups": []}).to_string()
        }
        "vault.set" => json!({
            "ok": true, "group": req["group"], "names": req["values"].as_object().unwrap().keys().collect::<Vec<_>>(),
            "replaced": false, "test": "ok", "tested": "login",
            "event": {"id": 3, "at": "2026-10-30T10:00:00.000Z", "actor": "ada", "group": req["group"],
                      "label": "Outreach mailbox (ADAUGO)", "action": "set",
                      "names": req["values"].as_object().unwrap().keys().collect::<Vec<_>>()},
        })
        .to_string(),
        "vault.test" => json!({"ok": true, "group": req["group"], "test": "ok", "tested": "login"}).to_string(),
        "vault.clear" => json!({
            "ok": true, "group": req["group"], "names": ["OUTREACH_CHINEDUM_SMTP_HOST"],
            "event": {"id": 4, "at": "2026-10-30T11:00:00.000Z", "actor": "chinedum", "group": req["group"],
                      "label": "Outreach mailbox (CHINEDUM)", "action": "clear", "names": ["OUTREACH_CHINEDUM_SMTP_HOST"]},
        })
        .to_string(),
        _ => json!({"ok": false, "error": "unknown_action"}).to_string(),
    }
    })
}

fn values(person: &str) -> Values {
    let mut m = Map::new();
    for (k, v) in [
        ("SMTP_HOST", "smtp.example.com"),
        ("SMTP_PORT", "465"),
        ("SMTP_USER", "ada@example.com"),
        ("SMTP_PASS", CANARY),
        ("IMAP_HOST", "imap.example.com"),
        ("IMAP_PORT", "993"),
        ("IMAP_USER", "ada@example.com"),
        ("IMAP_PASS", CANARY),
    ] {
        m.insert(format!("OUTREACH_{person}_{k}"), json!(v));
    }
    Values(m)
}

fn req(group: &str, values: Option<Values>) -> VaultRequest {
    VaultRequest {
        group: Some(group.into()),
        values,
    }
}

fn code(r: &Result<Value, VaultError>) -> String {
    r.as_ref().unwrap_err().code.clone()
}

// --- identity ----------------------------------------------------------------

#[tokio::test]
async fn the_approver_block_is_the_session_principal_and_the_source_is_shell() {
    let fake = FakeSender::start(sender("ADAUGO", false)).await;
    let (_, out) = relay(
        &fake.cfg(),
        &member(),
        Op::Set,
        req("outreach:ADAUGO", Some(values("ADAUGO"))),
    )
    .await;
    assert!(out.is_ok(), "{out:?}");
    let reqs = fake.requests();
    assert!(!reqs.is_empty());
    for r in &reqs {
        assert_eq!(r["source"], "shell");
        assert_eq!(
            r["approver"],
            json!({"kind": "principal", "id": "0192-ada", "username": "ada"})
        );
    }
}

#[test]
fn a_body_cannot_name_an_approver_a_role_or_a_person() {
    for field in [
        "approver",
        "principal",
        "username",
        "role",
        "source",
        "admin",
        "person",
    ] {
        let body = json!({ "group": "outreach:ADAUGO", field: "chinedum" }).to_string();
        let e = parse_body(body.as_bytes()).unwrap_err();
        assert_eq!(e.code, "invalid_request", "{field}");
    }
    assert!(parse_body(br#"{"group":"buffer","values":{"BUFFER_API_KEY":"x"}}"#).is_ok());
    assert!(parse_body(br#"{"values":{"lower":"x"}}"#).is_err());
    assert!(parse_body(br#"{"values":{"BUFFER_API_KEY":5}}"#).is_err());
}

// --- the shell's first layer -------------------------------------------------

#[tokio::test]
async fn a_member_cannot_target_another_person_or_a_shared_group() {
    let fake = FakeSender::start(sender("ADAUGO", false)).await;
    let cfg = fake.cfg();
    // Someone else's mailbox: set, test, clear, and even status.
    for op in [Op::Set, Op::Test, Op::Clear, Op::Status] {
        let (_, out) = relay(
            &cfg,
            &member(),
            op,
            req("outreach:CHINEDUM", Some(values("CHINEDUM"))),
        )
        .await;
        assert_eq!(code(&out), "not_permitted", "{op:?}");
    }
    let before = fake.actions();
    assert!(
        !before.iter().any(|a| a != "vault.status"),
        "only the person lookup may have reached the sender: {before:?}"
    );
    // A shared group: refused before any socket call at all.
    let n = fake.requests().len();
    for g in SHARED_GROUPS {
        for op in [Op::Set, Op::Test, Op::Clear, Op::Status] {
            let v = Values(
                [("BUFFER_API_KEY".to_string(), json!("x"))]
                    .into_iter()
                    .collect(),
            );
            let (_, out) = relay(&cfg, &member(), op, req(g, Some(v))).await;
            assert_eq!(code(&out), "not_permitted", "{g} {op:?}");
        }
    }
    assert_eq!(
        fake.requests().len(),
        n,
        "shared groups never reach the sender for a member"
    );
}

#[tokio::test]
async fn an_admin_clears_anyone_but_sets_only_their_own_mailbox() {
    let fake = FakeSender::start(sender("CHINEDUM", true)).await;
    let cfg = fake.cfg();
    let (a, out) = relay(&cfg, &admin(), Op::Clear, req("outreach:ADAUGO", None)).await;
    assert!(out.is_ok(), "{out:?}");
    assert_eq!(a.group.as_deref(), Some("outreach:ADAUGO"));
    assert!(a.event.is_some());
    for op in [Op::Set, Op::Test] {
        let (_, out) = relay(
            &cfg,
            &admin(),
            op,
            req("outreach:ADAUGO", Some(values("ADAUGO"))),
        )
        .await;
        assert_eq!(code(&out), "not_permitted", "{op:?}");
    }
    let (_, out) = relay(
        &cfg,
        &admin(),
        Op::Set,
        req("outreach:CHINEDUM", Some(values("CHINEDUM"))),
    )
    .await;
    assert!(out.is_ok(), "{out:?}");
    let v = Values(
        [
            ("BUFFER_API_KEY".to_string(), json!("x")),
            ("BUFFER_ORG_ID".to_string(), json!("o")),
        ]
        .into_iter()
        .collect(),
    );
    let (_, out) = relay(&cfg, &admin(), Op::Test, req("buffer", Some(v))).await;
    assert!(out.is_ok(), "{out:?}");
}

#[tokio::test]
async fn fields_must_belong_to_the_group_and_a_group_must_be_known() {
    let fake = FakeSender::start(sender("ADAUGO", false)).await;
    let cfg = fake.cfg();
    // A key of another person's mailbox smuggled into my group.
    let mut v = values("ADAUGO");
    v.0.insert("OUTREACH_CHINEDUM_SMTP_PASS".into(), json!("x"));
    let (_, out) = relay(&cfg, &member(), Op::Set, req("outreach:ADAUGO", Some(v))).await;
    assert_eq!(code(&out), "invalid_request");
    for bad in [
        "outreach:adaugo",
        "outreach:",
        "outreach:A",
        "other",
        "../buffer",
        "",
    ] {
        let (_, out) = relay(&cfg, &member(), Op::Clear, req(bad, None)).await;
        assert_eq!(code(&out), "invalid_group", "{bad:?}");
    }
    assert!(!fake.actions().iter().any(|a| a == "vault.set"));
    // No group on a write.
    let (_, out) = relay(
        &cfg,
        &member(),
        Op::Set,
        VaultRequest {
            group: None,
            values: None,
        },
    )
    .await;
    assert_eq!(code(&out), "invalid_request");
}

#[tokio::test]
async fn a_member_with_no_mailbox_slot_is_refused() {
    let fake = FakeSender::start(Arc::new(|_: &Value| {
        json!({"ok": true, "admin": false, "groups": []}).to_string()
    }))
    .await;
    let (_, out) = relay(
        &fake.cfg(),
        &member(),
        Op::Set,
        req("outreach:ADAUGO", Some(values("ADAUGO"))),
    )
    .await;
    assert_eq!(code(&out), "not_permitted");
}

// --- no value anywhere -------------------------------------------------------

#[tokio::test]
async fn no_response_audit_or_debug_output_contains_a_value() {
    // A misbehaving sender that echoes the values in every field it can.
    let echo: Handler = Arc::new(|req: &Value| {
        if req["action"] == "vault.status" && req.get("group").is_none() {
            return json!({"ok": true, "person": "ADAUGO", "admin": false, "groups": []})
                .to_string();
        }
        let leak = req.to_string();
        json!({
            "ok": true, "group": req["group"], "names": [format!("OUTREACH_ADAUGO_SMTP_HOST")],
            "replaced": true, "test": "ok", "tested": "login", "values": req["values"], "echo": leak,
            "message": leak, "label": leak,
            "event": {"id": 1, "at": "2026-10-30T10:00:00.000Z", "actor": "ada", "group": req["group"],
                      "label": leak, "action": "replace", "names": ["OUTREACH_ADAUGO_SMTP_HOST", leak], "values": req["values"]},
            "groups": [{"group": "outreach:ADAUGO", "label": leak, "shared": false,
                        "names": [{"name": "OUTREACH_ADAUGO_SMTP_PASS", "required": true}, {"name": leak, "required": true}],
                        "present": {"OUTREACH_ADAUGO_SMTP_PASS": 14, "OUTREACH_ADAUGO_IMAP_PASS": leak},
                        "missing": [leak], "complete": false, "lastChanged": leak, "lastChangedBy": "ada"}],
        })
        .to_string()
    });
    let fake = FakeSender::start(echo).await;
    let cfg = fake.cfg();
    let mut all = String::new();
    for op in [Op::Set, Op::Test, Op::Status] {
        let r = req("outreach:ADAUGO", Some(values("ADAUGO")));
        let (audit, out) = relay(&cfg, &member(), op, r).await;
        all += &format!("{audit:?}{out:?}");
        if let Some(e) = &audit.event {
            all += &founder_note(e, "ada");
        }
    }
    assert!(
        all.contains("OUTREACH_ADAUGO_SMTP_HOST"),
        "sanity: names do appear"
    );
    assert!(!all.contains(CANARY), "a value leaked: {all}");
    // The request side: a Values never prints its contents.
    assert!(!format!("{:?}", values("ADAUGO")).contains(CANARY));
    assert!(!format!("{:?}", req("outreach:ADAUGO", Some(values("ADAUGO")))).contains(CANARY));
}

#[tokio::test]
async fn an_error_carries_fixed_text_never_the_senders_words() {
    let h: Handler = Arc::new(|req: &Value| {
        if req["action"] == "vault.status" {
            return json!({"ok": true, "person": "ADAUGO", "admin": false}).to_string();
        }
        json!({"ok": false, "error": "test_failed", "test": "smtp_auth_failed", "tested": "login",
               "group": "outreach:ADAUGO", "message": CANARY, "detail": CANARY, "reason": CANARY, "names": [CANARY]})
        .to_string()
    });
    let fake = FakeSender::start(h).await;
    let (a, out) = relay(
        &fake.cfg(),
        &member(),
        Op::Set,
        req("outreach:ADAUGO", Some(values("ADAUGO"))),
    )
    .await;
    let e = out.unwrap_err();
    assert_eq!(e.code, "test_failed");
    assert_eq!(e.extra["test"], "smtp_auth_failed");
    assert_eq!(a.result, "test_failed:smtp_auth_failed");
    let body = format!("{e:?}{a:?}");
    assert!(!body.contains(CANARY));
    // And with a rendered HTTP body.
    let resp = e.into_response();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 16)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains(CANARY));
}

// --- sender errors -------------------------------------------------------------

#[tokio::test]
async fn sender_codes_map_to_statuses_and_fixed_extras() {
    for (sender_code, status, extra) in [
        ("vault_unconfigured", StatusCode::SERVICE_UNAVAILABLE, None),
        ("unauthorized_approver", StatusCode::FORBIDDEN, None),
        ("not_permitted", StatusCode::FORBIDDEN, None),
        (
            "rate_limited",
            StatusCode::TOO_MANY_REQUESTS,
            Some(("retryAfterSec", json!(120))),
        ),
        ("busy", StatusCode::SERVICE_UNAVAILABLE, None),
        ("invalid_value", StatusCode::BAD_REQUEST, None),
        ("vault_unsafe", StatusCode::INTERNAL_SERVER_ERROR, None),
        ("Some Weird Text!", StatusCode::BAD_GATEWAY, None),
    ] {
        let h: Handler = Arc::new(move |req: &Value| {
            if req["action"] == "vault.status" {
                return json!({"ok": true, "person": "ADAUGO"}).to_string();
            }
            json!({"ok": false, "error": sender_code, "retryAfterSec": 120}).to_string()
        });
        let fake = FakeSender::start(h).await;
        let (_, out) = relay(
            &fake.cfg(),
            &member(),
            Op::Clear,
            req("outreach:ADAUGO", None),
        )
        .await;
        let e = out.unwrap_err();
        assert_eq!(e.status, status, "{sender_code}");
        if let Some((k, v)) = extra {
            assert_eq!(e.extra[k], v);
        }
        if sender_code == "Some Weird Text!" {
            assert_eq!(e.code, "sender_error");
        }
    }
}

#[tokio::test]
async fn an_unreachable_or_garbled_sender_fails_closed() {
    let cfg = ApprovalsConfig {
        socket: PathBuf::from("/nonexistent/approvals.sock"),
        agent_uid_min: u32::MAX,
        sender_uid: Some(me()),
        ..ApprovalsConfig::default()
    };
    let (_, out) = relay(&cfg, &member(), Op::Clear, req("outreach:ADAUGO", None)).await;
    assert_eq!(code(&out), "sender_unavailable");

    let fake = FakeSender::start(Arc::new(|_: &Value| "not json".to_string())).await;
    let (_, out) = relay(
        &fake.cfg(),
        &admin(),
        Op::Clear,
        req("outreach:ADAUGO", None),
    )
    .await;
    // A write whose reply cannot be read may still have been applied.
    assert_eq!(code(&out), "outcome_unknown");
    // A test or a status changes nothing: the plain wording.
    let (_, out) = relay(
        &fake.cfg(),
        &admin(),
        Op::Test,
        req("outreach:ADAUGO", Some(values("ADAUGO"))),
    )
    .await;
    assert_eq!(code(&out), "sender_error");
}

// --- status, note ------------------------------------------------------------------

#[test]
fn status_keeps_only_names_integer_lengths_and_times() {
    let out = sanitize_status(&json!({
        "ok": true, "person": "ADAUGO", "admin": false,
        "groups": [{"group": "outreach:ADAUGO", "label": "Outreach mailbox (ADAUGO)", "shared": false,
            "names": [{"name": "OUTREACH_ADAUGO_SMTP_HOST", "required": true}],
            "present": {"OUTREACH_ADAUGO_SMTP_HOST": 16, "OUTREACH_ADAUGO_SMTP_PASS": CANARY},
            "missing": ["OUTREACH_ADAUGO_IMAP_PASS"], "complete": false,
            "lastChanged": "2026-10-30T10:00:00.000Z", "lastChangedBy": "ada",
            "readiness": {"identityKnown": true, "approverListed": null, "x": CANARY}},
            {"group": "evil", "label": CANARY}]
    }));
    assert_eq!(out["person"], "ADAUGO");
    assert_eq!(out["groups"].as_array().unwrap().len(), 1);
    let g = &out["groups"][0];
    assert_eq!(g["present"], json!({"OUTREACH_ADAUGO_SMTP_HOST": 16}));
    assert_eq!(
        g["readiness"],
        json!({"identityKnown": true, "approverListed": null})
    );
    assert!(!out.to_string().contains(CANARY));
}

#[test]
fn the_founder_note_names_keys_and_never_values() {
    let note = founder_note(
        &json!({"actor": "ada", "action": "replace", "label": "Outreach mailbox (ADAUGO)",
                "at": "2026-10-30T10:00:00.000Z", "names": ["OUTREACH_ADAUGO_SMTP_PASS"]}),
        "x",
    );
    assert!(note.starts_with("ada replaced Outreach mailbox (ADAUGO)"));
    assert!(note.contains("OUTREACH_ADAUGO_SMTP_PASS"));
    assert!(note.contains("key names only"));
}

#[test]
fn the_audit_kind_is_registered() {
    assert!(crate::access::audit::category_of(AUDIT_KIND).is_some());
}

#[test]
fn authorise_matrix() {
    let own = Group::Outreach("ADAUGO".into());
    let other = Group::Outreach("CHINEDUM".into());
    let shared = Group::Shared("buffer".into());
    let m = member();
    let a = admin();
    for op in [Op::Status, Op::Set, Op::Test, Op::Clear] {
        assert!(authorise(op, &own, &m, Some("ADAUGO")).is_ok());
        assert!(authorise(op, &other, &m, Some("ADAUGO")).is_err());
        assert!(authorise(op, &shared, &m, Some("ADAUGO")).is_err());
        assert!(authorise(op, &shared, &a, Some("CHINEDUM")).is_ok());
    }
    assert!(authorise(Op::Clear, &other, &a, Some("ADAUGO")).is_ok());
    assert!(authorise(Op::Status, &other, &a, Some("ADAUGO")).is_ok());
    assert!(authorise(Op::Set, &other, &a, Some("ADAUGO")).is_err());
    assert!(authorise(Op::Test, &other, &a, Some("ADAUGO")).is_err());
}

// --- repair round: admission, uid pin, unknown outcome, lengths ---------------

fn admission() -> Admission {
    Admission {
        account_enabled: true,
        strong: true,
        in_share: false,
        has_settings: true,
    }
}

#[tokio::test]
async fn the_handler_refuses_a_share_a_weak_credential_a_disabled_account_and_no_settings_cap() {
    let fake = FakeSender::start(sender("ADAUGO", false)).await;
    let body = Bytes::from(json!({"group": "outreach:ADAUGO"}).to_string());
    let ok = admission();
    let cases = [
        Admission {
            in_share: true,
            ..ok
        },
        Admission {
            strong: false,
            ..ok
        },
        Admission {
            account_enabled: false,
            ..ok
        },
        Admission {
            has_settings: false,
            ..ok
        },
    ];
    for a in cases {
        for op in [Op::Status, Op::Set, Op::Test, Op::Clear] {
            let (audit, out) = handle(&fake.cfg(), &a, &member(), op, &body).await;
            assert_eq!(code(&out), "forbidden", "{a:?} {op:?}");
            assert_eq!(audit.result, "forbidden");
        }
    }
    assert!(
        fake.requests().is_empty(),
        "a refusal never reaches the sender"
    );
    // The same request with a good admission does.
    let (_, out) = handle(&fake.cfg(), &ok, &member(), Op::Clear, &body).await;
    assert!(out.is_ok(), "{out:?}");
}

#[tokio::test]
async fn the_handler_refuses_an_extra_body_field_before_any_socket_call() {
    let fake = FakeSender::start(sender("ADAUGO", false)).await;
    let body = Bytes::from(
        json!({"group": "outreach:ADAUGO", "approver": {"id": "x", "username": "chinedum"}})
            .to_string(),
    );
    let (_, out) = handle(&fake.cfg(), &admission(), &member(), Op::Clear, &body).await;
    assert_eq!(code(&out), "invalid_request");
    assert!(fake.requests().is_empty());
}

#[tokio::test]
async fn the_socket_must_be_served_by_the_configured_sender_uid() {
    let fake = FakeSender::start(sender("ADAUGO", false)).await;
    // Served by this process's uid, but the sender is configured as another.
    let wrong = ApprovalsConfig {
        sender_uid: Some(me() + 1),
        ..fake.cfg()
    };
    let (_, out) = relay(&wrong, &member(), Op::Clear, req("outreach:ADAUGO", None)).await;
    assert_eq!(code(&out), "sender_unavailable");
    assert!(
        fake.requests().is_empty(),
        "nothing is sent to a wrong-uid socket"
    );
    // No sender uid configured at all: fail closed.
    let none = ApprovalsConfig {
        sender_uid: None,
        ..fake.cfg()
    };
    let (_, out) = relay(
        &none,
        &member(),
        Op::Status,
        VaultRequest {
            group: None,
            values: None,
        },
    )
    .await;
    assert_eq!(code(&out), "sender_unavailable");
    assert!(fake.requests().is_empty());
}

#[tokio::test]
async fn a_write_that_gets_no_answer_is_an_unknown_outcome_not_nothing_changed() {
    // The sender takes the request and hangs up without a reply.
    let h: Handler = Arc::new(|req: &Value| {
        if req["action"] == "vault.status" {
            json!({"ok": true, "person": "ADAUGO"}).to_string()
        } else {
            String::new()
        }
    });
    let fake = FakeSender::start(h).await;
    for op in [Op::Set, Op::Clear] {
        let r = req("outreach:ADAUGO", Some(values("ADAUGO")));
        let (_, out) = relay(&fake.cfg(), &member(), op, r).await;
        let e = out.unwrap_err();
        assert_eq!(e.code, "outcome_unknown", "{op:?}");
        assert!(e.message.contains("may or may not have been saved"));
        assert!(!e.message.contains("Nothing was changed"));
    }
    // A test never changes anything, so it keeps the plain wording.
    let (_, out) = relay(
        &fake.cfg(),
        &member(),
        Op::Test,
        req("outreach:ADAUGO", Some(values("ADAUGO"))),
    )
    .await;
    assert_eq!(code(&out), "sender_error");
    // A timeout after the request was sent maps the same way for a write.
    let e = map_transport(
        &SenderError::Unavailable("the sender did not answer".into()),
        true,
    );
    assert_eq!(e.code, "outcome_unknown");
    let e = map_transport(
        &SenderError::Unavailable("connect /x: refused".into()),
        true,
    );
    assert_eq!(e.code, "sender_unavailable");
}

#[test]
fn other_peoples_password_lengths_are_not_shown_to_an_admin() {
    let g = |p: &str| {
        json!({"group": format!("outreach:{p}"), "label": "x", "shared": false, "names": [],
               "present": {format!("OUTREACH_{p}_SMTP_PASS"): 14}, "missing": [], "complete": false})
    };
    let shared = json!({"group": "buffer", "label": "Buffer", "shared": true, "names": [],
                        "present": {"BUFFER_API_KEY": 40}, "missing": [], "complete": true});
    let out = sanitize_status(&json!({
        "ok": true, "person": "CHINEDUM", "admin": true, "groups": [g("CHINEDUM"), g("ADAUGO"), shared]
    }));
    let groups = out["groups"].as_array().unwrap();
    assert_eq!(groups[0]["present"]["OUTREACH_CHINEDUM_SMTP_PASS"], 14);
    assert!(groups[1]["present"]["OUTREACH_ADAUGO_SMTP_PASS"].is_null());
    assert!(groups[1]["present"]
        .as_object()
        .unwrap()
        .contains_key("OUTREACH_ADAUGO_SMTP_PASS"));
    assert_eq!(groups[2]["present"]["BUFFER_API_KEY"], 40);
}
