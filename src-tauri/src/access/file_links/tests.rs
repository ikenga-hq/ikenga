//! box-files WP-15: the link store and its five arms (06-file-links §3, §7),
//! against a T1 access store and a fake owner child.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};

use super::*;
use crate::access::caps::{CapSet, Tier};
use crate::access::ctx::{RequestMeta, ShareCtx, Via};
use crate::access::policy::tests::{session, t1_store};
use crate::access::share::BoxFuture;
use crate::access::Role;

const BASE: &str = "https://box.example";

/// An owner child that knows a few files per account. Every call is
/// recorded with the principal it was sent to.
#[derive(Default)]
struct FakeChild {
    files: Mutex<HashMap<(String, String), Value>>,
    calls: Mutex<Vec<(String, String, Value)>>,
}

impl FakeChild {
    fn with(owner: PrincipalId, path: &str, name: &str, size: u64) -> Self {
        let c = Self::default();
        c.add(owner, path, name, size);
        c
    }

    fn add(&self, owner: PrincipalId, path: &str, name: &str, size: u64) {
        // A hostile child could claim any mime: the broker must ignore it.
        self.files.lock().unwrap().insert(
            (owner.to_string(), path.to_string()),
            json!({"path": path, "name": name, "size": size,
                   "mime": "text/html", "previewKind": "pdf"}),
        );
    }

    fn calls(&self) -> Vec<(String, String, Value)> {
        self.calls.lock().unwrap().clone()
    }
}

impl OwnerCalls for FakeChild {
    fn call<'a>(
        &'a self,
        owner: PrincipalId,
        cmd: &'a str,
        args: Value,
    ) -> BoxFuture<'a, Result<Value, AccessError>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push((owner.to_string(), cmd.to_string(), args.clone()));
            if cmd != RESOLVE_ARM {
                return Err(AccessError::new(Code::NotFound, "no such arm"));
            }
            let path = args["path"].as_str().unwrap_or_default().to_string();
            self.files
                .lock()
                .unwrap()
                .get(&(owner.to_string(), path))
                .cloned()
                .ok_or_else(|| {
                    AccessError::new(Code::NotFound, "no such file you can share by link")
                })
        })
    }

    fn close_principal(&self, _principal: PrincipalId) -> usize {
        0
    }
}

fn on() -> AccessOptions {
    AccessOptions {
        public_url: Some(BASE.into()),
        file_links: true,
        ..AccessOptions::default()
    }
}

fn device(owner: PrincipalId, tier: Tier, caps: CapSet) -> AccessCtx {
    AccessCtx {
        principal_id: owner,
        via: Via::Device {
            device_id: "dev-1".into(),
        },
        device_id: Some("dev-1".into()),
        tier,
        share: None,
        share_headers: false,
        caps,
        admin_strength: AccessCtx::admin_strength_of(
            &Via::Device {
                device_id: "dev-1".into(),
            },
            tier,
        ),
        is_admin: false,
        meta: RequestMeta::default(),
    }
}

fn share_of(owner: PrincipalId) -> ShareCtx {
    ShareCtx {
        project_key: format!("{owner}/royalti-co"),
        project_id: "royalti-co".into(),
        member_principal_id: None,
        member_device_id: None,
        role: Some(Role::Operator),
        artifact_path: None,
        owner_approval: false,
    }
}

async fn create_ok(store: &AccessStore, ctx: &AccessCtx, child: &FakeChild, args: Value) -> Value {
    create(store, ctx, &on(), child, &args, now_ms())
        .await
        .unwrap()
}

fn token_of(v: &Value) -> String {
    v["url"]
        .as_str()
        .unwrap()
        .strip_prefix(&format!("{BASE}/s/"))
        .unwrap()
        .to_string()
}

/// A row straight into the table (for the cap and listing tests).
async fn insert_raw(store: &AccessStore, owner: PrincipalId, expires_at: Option<i64>) -> String {
    let id = PrincipalId::new_v7().to_string();
    let now = now_ms();
    sqlx::query(
        "INSERT INTO file_links (link_id, token_sha256, owner_principal_id, path, name, \
           preview_kind, mime, size_at_create, created_at, created_via, expires_at) \
         VALUES (?, ?, ?, '/home/a/r.pdf', 'r.pdf', 'pdf', 'application/pdf', 1, ?, \
           'session', ?)",
    )
    .bind(&id)
    .bind(mint_token().sha256.as_slice())
    .bind(owner.to_string())
    .bind(now - 1000)
    .bind(expires_at)
    .execute(store.pool())
    .await
    .unwrap();
    id
}

async fn kinds(store: &AccessStore) -> Vec<String> {
    sqlx::query_scalar("SELECT kind FROM audit_events WHERE kind LIKE 'link.%' ORDER BY seq")
        .fetch_all(store.pool())
        .await
        .unwrap()
}

// ─── pure rules ─────────────────────────────────────────────────────────────

#[test]
fn tokens_are_43_url_safe_chars_and_hashed() {
    let m = mint_token();
    assert_eq!(m.token.len(), TOKEN_LEN);
    assert!(looks_like_token(&m.token));
    assert_eq!(token_hash(&m.token), m.sha256);
    assert_ne!(mint_token().token, m.token);
    assert!(!looks_like_token(&m.token[..42]));
    assert!(!looks_like_token(&format!("{}=", &m.token[..42])));
    assert!(!looks_like_token(&format!("{}.", &m.token[..42])));
    assert!(!looks_like_token(&format!(" {}", &m.token[..42])));
    assert_eq!(link_url("https://b.example/", "t"), "https://b.example/s/t");
}

#[test]
fn the_preview_table_never_names_html_or_svg() {
    use PreviewKind as K;
    let mib = 1024 * 1024;
    assert_eq!(classify("r.pdf", 10), (K::Pdf, "application/pdf"));
    assert_eq!(classify("R.PDF", 10), (K::Pdf, "application/pdf"));
    assert_eq!(classify("a.png", 10), (K::Image, "image/png"));
    assert_eq!(classify("a.JPG", 10), (K::Image, "image/jpeg"));
    assert_eq!(classify("a.jpeg", 10), (K::Image, "image/jpeg"));
    assert_eq!(classify("a.gif", 10), (K::Image, "image/gif"));
    assert_eq!(classify("a.webp", 10), (K::Image, "image/webp"));
    for t in ["a.txt", "a.md", "a.markdown", "a.csv", "a.log"] {
        assert_eq!(
            classify(t, 10),
            (K::Text, "text/plain; charset=utf-8"),
            "{t}"
        );
    }
    for n in [
        "x.html", "x.htm", "x.svg", "x.xml", "x.js", "x.docx", "noext", ".pdf",
    ] {
        assert_eq!(classify(n, 10), (K::None, OCTET_STREAM), "{n}");
    }
    // Over the preview limit → download only.
    assert_eq!(classify("a.png", 25 * mib + 1).0, K::None);
    assert_eq!(classify("a.png", 25 * mib).0, K::Image);
    assert_eq!(classify("a.md", 5 * mib + 1).0, K::None);
    assert_eq!(classify("a.pdf", LINK_MAX_BYTES).0, K::Pdf);
    for k in [K::Pdf, K::Image, K::Text, K::None] {
        assert_eq!(PreviewKind::parse(k.as_str()), Some(k));
    }
}

/// LS-22: bidi overrides and controls never reach the stored name.
#[test]
fn names_lose_controls_and_bidi_overrides() {
    assert_eq!(clean_name("invoice\u{202E}fdp.exe"), "invoicefdp.exe");
    assert_eq!(clean_name("a\u{2066}b\u{2069}c\u{200F}.txt"), "abc.txt");
    assert_eq!(clean_name("bad\r\nname\u{0}\u{85}.pdf"), "badname.pdf");
    assert_eq!(clean_name("a/b\\c.pdf"), "a_b_c.pdf");
    assert_eq!(clean_name("  \u{202E}  "), "download");
    assert_eq!(clean_name(".."), "download");
    let long = "é".repeat(400);
    assert_eq!(clean_name(&long).chars().count(), 255);
    // And it is the cleaned name that is classified.
    assert_eq!(
        Resolved::from_parts("/a".into(), "x.pdf\u{202E}", 1).name,
        "x.pdf"
    );
}

#[test]
fn create_args_are_validated() {
    let now = 1_000;
    let spec = |v: Value| create_spec(&v, now);
    let d = spec(json!({"path": "/h/r.pdf"})).unwrap();
    assert_eq!(d.expires_at, Some(now + DEFAULT_EXPIRY_DAYS * DAY_MS));
    assert_eq!(d.passcode, None);
    assert_eq!(
        spec(json!({"path": "/h/r.pdf", "never": true}))
            .unwrap()
            .expires_at,
        None
    );
    assert_eq!(
        spec(json!({"path": "/h/r.pdf", "expiresInDays": 30}))
            .unwrap()
            .expires_at,
        Some(now + 30 * DAY_MS)
    );
    for bad in [
        json!({}),
        json!({"path": ""}),
        json!({"path": "/h/r.pdf", "expiresInDays": 0}),
        json!({"path": "/h/r.pdf", "expiresInDays": 366}),
        json!({"path": "/h/r.pdf", "expiresInDays": 1.5}),
        json!({"path": "/h/r.pdf", "expiresInDays": "7"}),
        json!({"path": "/h/r.pdf", "expiresInDays": 7, "never": true}),
        json!({"path": "/h/r.pdf", "never": "yes"}),
        json!({"path": "/h/r.pdf", "passcode": "12345"}),
        json!({"path": "/h/r.pdf", "passcode": "x".repeat(129)}),
        json!({"path": "/h/r.pdf", "passcode": 123456}),
    ] {
        assert_eq!(
            spec(bad.clone()).unwrap_err().code,
            Code::InvalidRequest,
            "{bad}"
        );
    }
    assert_eq!(
        spec(json!({"path": "/h/r.pdf", "passcode": ""}))
            .unwrap()
            .passcode,
        None
    );
    assert_eq!(
        spec(json!({"path": "/h/r.pdf", "passcode": "é".repeat(6)}))
            .unwrap()
            .passcode,
        Some("é".repeat(6))
    );
}

#[test]
fn states() {
    assert_eq!(link_state(None, None, None, 10), "live");
    assert_eq!(link_state(Some(5), None, None, 10), "revoked");
    assert_eq!(link_state(None, Some(10), None, 10), "expired");
    assert_eq!(link_state(None, Some(20), Some(15), 10), "locked");
    assert_eq!(link_state(None, Some(20), Some(5), 10), "live");
}

#[test]
fn the_child_answer_is_rechecked() {
    let ok = Resolved::from_child(&json!({"path": "/h/x.svg", "name": "x.svg", "size": 3,
        "mime": "image/svg+xml", "previewKind": "image"}))
    .unwrap();
    assert_eq!(
        (ok.preview_kind, ok.mime),
        (PreviewKind::None, OCTET_STREAM)
    );
    for bad in [
        json!({"path": "h/x.pdf", "name": "x.pdf", "size": 1}),
        json!({"path": "/", "name": "x.pdf", "size": 1}),
        json!({"name": "x.pdf", "size": 1}),
        json!({"path": "/h/x.pdf", "size": 1}),
        json!({"path": "/h/x.pdf", "name": "x.pdf", "size": -1}),
    ] {
        assert!(Resolved::from_child(&bad).is_err(), "{bad}");
    }
    assert_eq!(
        Resolved::from_child(&json!({"path": "/h/b", "name": "b", "size": LINK_MAX_BYTES + 1}))
            .unwrap_err()
            .code,
        Code::NotFound
    );
}

// ─── create ─────────────────────────────────────────────────────────────────

/// The token is 43 chars, only its SHA-256 is stored, and it appears in no
/// row anywhere; the resolve went to the creator's own child.
#[tokio::test]
async fn create_stores_only_the_hash_and_asks_the_creators_child() {
    let store = t1_store().await;
    let owner = PrincipalId::new_v7();
    let child = FakeChild::with(owner, "/home/ned/r.pdf", "r.pdf", 10);
    let out = create_ok(
        &store,
        &session(owner),
        &child,
        json!({"path": "/home/ned/r.pdf"}),
    )
    .await;
    let token = token_of(&out);
    assert_eq!(token.len(), TOKEN_LEN);
    assert!(looks_like_token(&token));
    assert_eq!(out["name"], "r.pdf");
    assert_eq!(out["size"], 10);
    assert_eq!(out["previewKind"], "pdf");
    assert_eq!(out["hasPasscode"], false);
    assert!(out["expiresAt"].as_i64().unwrap() > now_ms() + 6 * DAY_MS);

    let (hash, mime, owner_col, via): (Vec<u8>, String, String, String) = sqlx::query_as(
        "SELECT token_sha256, mime, owner_principal_id, created_via FROM file_links WHERE link_id = ?",
    )
    .bind(out["linkId"].as_str().unwrap())
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(hash, token_hash(&token).to_vec());
    assert_eq!(
        mime, "application/pdf",
        "the child's claimed text/html is ignored"
    );
    assert_eq!(owner_col, owner.to_string());
    assert_eq!(via, "session");

    let leaks: i64 = sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM audit_events WHERE \
             COALESCE(detail, '') || COALESCE(target, '') LIKE '%' || ?1 || '%') + \
           (SELECT COUNT(*) FROM file_links WHERE \
             path || name || COALESCE(passcode_phc, '') LIKE '%' || ?1 || '%')",
    )
    .bind(&token)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(leaks, 0, "the token is nowhere at rest");

    let calls = child.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, owner.to_string());
    assert_eq!(calls[0].1, RESOLVE_ARM);
    assert_eq!(kinds(&store).await, vec!["link.created"]);
}

#[tokio::test]
async fn create_from_a_full_device_records_the_device_and_never_expires() {
    let store = t1_store().await;
    let owner = PrincipalId::new_v7();
    let child = FakeChild::with(owner, "/home/ned/a.png", "a.png", 10);
    let ctx = device(owner, Tier::Full, CapSet::ALL);
    let out = create_ok(
        &store,
        &ctx,
        &child,
        json!({"path": "/home/ned/a.png", "never": true}),
    )
    .await;
    assert_eq!(out["expiresAt"], Value::Null);
    assert_eq!(out["previewKind"], "image");
    let (via, dev): (String, Option<String>) =
        sqlx::query_as("SELECT created_via, created_device_id FROM file_links WHERE link_id = ?")
            .bind(out["linkId"].as_str().unwrap())
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!((via.as_str(), dev.as_deref()), ("device", Some("dev-1")));
}

#[tokio::test]
async fn the_setting_and_the_public_url_gate_create() {
    let store = t1_store().await;
    let owner = PrincipalId::new_v7();
    let child = FakeChild::with(owner, "/home/ned/r.pdf", "r.pdf", 10);
    let args = json!({"path": "/home/ned/r.pdf"});
    let off = AccessOptions {
        public_url: Some(BASE.into()),
        ..AccessOptions::default()
    };
    let e = create(&store, &session(owner), &off, &child, &args, now_ms())
        .await
        .unwrap_err();
    assert_eq!(e.code, Code::Forbidden);
    assert!(e.message.contains("turned off"), "{}", e.message);
    let no_url = AccessOptions {
        file_links: true,
        ..AccessOptions::default()
    };
    let e = create(&store, &session(owner), &no_url, &child, &args, now_ms())
        .await
        .unwrap_err();
    assert_eq!(e.code, Code::Forbidden);
    assert!(
        child.calls().is_empty(),
        "refused before the child is asked"
    );
    // The list still works with the setting off (nothing in it yet).
    let l = list(&store, &session(owner), now_ms()).await.unwrap();
    assert_eq!(l["links"].as_array().unwrap().len(), 0);
}

/// LS-07: a share selection never creates; a member passing the owner's
/// absolute path is resolved on the member's own child, which knows nothing.
#[tokio::test]
async fn a_share_never_creates_and_another_accounts_path_is_not_found() {
    let store = t1_store().await;
    let owner = PrincipalId::new_v7();
    let member = PrincipalId::new_v7();
    let child = FakeChild::with(owner, "/home/owner/secret.pdf", "secret.pdf", 10);
    let args = json!({"path": "/home/owner/secret.pdf"});

    let mut shared = session(member);
    shared.share = Some(share_of(owner));
    let e = create(&store, &shared, &on(), &child, &args, now_ms())
        .await
        .unwrap_err();
    assert_eq!(e.code, Code::Forbidden);
    let mut headers_only = session(member);
    headers_only.share_headers = true;
    let e = create(&store, &headers_only, &on(), &child, &args, now_ms())
        .await
        .unwrap_err();
    assert_eq!(e.code, Code::Forbidden);
    assert!(child.calls().is_empty());

    let e = create(&store, &session(member), &on(), &child, &args, now_ms())
        .await
        .unwrap_err();
    assert_eq!(e.code, Code::NotFound);
    assert_eq!(
        child.calls()[0].0,
        member.to_string(),
        "resolved on the member's own child"
    );
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM file_links")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(n, 0);
    // The share refusal holds for every owner arm.
    for cmd in ["list", "revoke"] {
        let e = match cmd {
            "list" => list(&store, &shared, now_ms()).await.unwrap_err(),
            _ => revoke(&store, &shared, &json!({"linkId": "x"}), now_ms())
                .await
                .unwrap_err(),
        };
        assert_eq!(e.code, Code::Forbidden, "{cmd}");
    }
}

/// Owner arms need `files` and admin strength: a view-tier phone can't
/// publish, list or revoke.
#[tokio::test]
async fn owner_arms_need_files_and_admin_strength() {
    let store = t1_store().await;
    let owner = PrincipalId::new_v7();
    let child = FakeChild::with(owner, "/home/ned/r.pdf", "r.pdf", 10);
    let args = json!({"path": "/home/ned/r.pdf"});
    let phone = device(owner, Tier::View, Tier::View.caps());
    assert!(!phone.admin_strength);
    let e = create(&store, &phone, &on(), &child, &args, now_ms())
        .await
        .unwrap_err();
    assert_eq!(e.code, Code::Forbidden);
    assert_eq!(
        list(&store, &phone, now_ms()).await.unwrap_err().code,
        Code::Forbidden
    );
    let no_files = device(owner, Tier::Full, CapSet::of(&[Cap::Sessions]));
    let e = create(&store, &no_files, &on(), &child, &args, now_ms())
        .await
        .unwrap_err();
    assert_eq!(e.code, Code::Forbidden);
    assert!(e.message.contains("missing="), "{}", e.message);
    assert!(child.calls().is_empty());
}

/// §3: at most 200 live links; expired and revoked ones don't count.
#[tokio::test]
async fn two_hundred_live_links_is_the_cap() {
    let store = t1_store().await;
    let owner = PrincipalId::new_v7();
    let child = FakeChild::with(owner, "/home/ned/r.pdf", "r.pdf", 10);
    let args = json!({"path": "/home/ned/r.pdf"});
    let mut ids = Vec::new();
    for _ in 0..MAX_LIVE_LINKS {
        ids.push(insert_raw(&store, owner, None).await);
    }
    // Someone else's links never count against this account.
    insert_raw(&store, PrincipalId::new_v7(), None).await;
    let e = create(&store, &session(owner), &on(), &child, &args, now_ms())
        .await
        .unwrap_err();
    assert_eq!(e.code, Code::Conflict);
    sqlx::query("UPDATE file_links SET expires_at = created_at + 1 WHERE link_id = ?")
        .bind(&ids[0])
        .execute(store.pool())
        .await
        .unwrap();
    create_ok(&store, &session(owner), &child, args.clone()).await;
    let e = create(&store, &session(owner), &on(), &child, &args, now_ms())
        .await
        .unwrap_err();
    assert_eq!(e.code, Code::Conflict);
    revoke(
        &store,
        &session(owner),
        &json!({"linkId": ids[1]}),
        now_ms(),
    )
    .await
    .unwrap();
    create_ok(&store, &session(owner), &child, args).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_passcode_is_stored_as_argon2id_only() {
    let store = t1_store().await;
    let owner = PrincipalId::new_v7();
    let child = FakeChild::with(owner, "/home/ned/r.pdf", "r.pdf", 10);
    let out = create_ok(
        &store,
        &session(owner),
        &child,
        json!({"path": "/home/ned/r.pdf", "passcode": "open-sesame"}),
    )
    .await;
    assert_eq!(out["hasPasscode"], true);
    let phc: String = sqlx::query_scalar("SELECT passcode_phc FROM file_links WHERE link_id = ?")
        .bind(out["linkId"].as_str().unwrap())
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert!(phc.starts_with("$argon2id$"), "{phc}");
    assert!(!phc.contains("open-sesame"));
    assert!(crate::server::operator::password::verify_blocking(
        &phc,
        "open-sesame"
    ));
    let detail: String =
        sqlx::query_scalar("SELECT detail FROM audit_events WHERE kind = 'link.created'")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert!(!detail.contains("open-sesame"));
}

// ─── list and revoke ────────────────────────────────────────────────────────

#[tokio::test]
async fn list_shows_own_links_without_secrets_and_hides_old_ended_ones() {
    let store = t1_store().await;
    let owner = PrincipalId::new_v7();
    let child = FakeChild::with(owner, "/home/ned/r.pdf", "r.pdf", 10);
    let live = create_ok(
        &store,
        &session(owner),
        &child,
        json!({"path": "/home/ned/r.pdf"}),
    )
    .await;
    insert_raw(&store, PrincipalId::new_v7(), None).await; // another account's
    let old = insert_raw(&store, owner, None).await;
    let now = now_ms();
    // Expired 40 days ago: out of the list. Expired yesterday: in it.
    sqlx::query("UPDATE file_links SET created_at = ?, expires_at = ? WHERE link_id = ?")
        .bind(now - 50 * DAY_MS)
        .bind(now - 40 * DAY_MS)
        .bind(&old)
        .execute(store.pool())
        .await
        .unwrap();
    let recent = insert_raw(&store, owner, None).await;
    sqlx::query("UPDATE file_links SET created_at = ?, expires_at = ? WHERE link_id = ?")
        .bind(now - 2 * DAY_MS)
        .bind(now - DAY_MS)
        .bind(&recent)
        .execute(store.pool())
        .await
        .unwrap();
    let l = list(&store, &session(owner), now).await.unwrap();
    let links = l["links"].as_array().unwrap();
    let ids: Vec<&str> = links
        .iter()
        .map(|x| x["linkId"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&live["linkId"].as_str().unwrap()));
    assert!(!ids.contains(&old.as_str()));
    assert_eq!(links.len(), 2, "{ids:?}");
    for x in links {
        let o = x.as_object().unwrap();
        for k in o.keys() {
            assert!(
                !k.to_lowercase().contains("token") && !k.contains("sha") && k != "passcodePhc",
                "{k}"
            );
        }
    }
    let st: Vec<&str> = links.iter().map(|x| x["state"].as_str().unwrap()).collect();
    assert!(st.contains(&"live") && st.contains(&"expired"), "{st:?}");
    let text = l.to_string();
    assert!(!text.contains(&token_of(&live)));
}

/// Another account's id answers exactly what an unknown id does.
#[tokio::test]
async fn revoke_is_owner_only_and_idempotent() {
    let store = t1_store().await;
    let owner = PrincipalId::new_v7();
    let child = FakeChild::with(owner, "/home/ned/r.pdf", "r.pdf", 10);
    let out = create_ok(
        &store,
        &session(owner),
        &child,
        json!({"path": "/home/ned/r.pdf"}),
    )
    .await;
    let id = out["linkId"].as_str().unwrap();
    let stranger = session(PrincipalId::new_v7());
    let theirs = revoke(&store, &stranger, &json!({"linkId": id}), now_ms())
        .await
        .unwrap_err();
    let unknown = revoke(
        &store,
        &stranger,
        &json!({"linkId": PrincipalId::new_v7().to_string()}),
        now_ms(),
    )
    .await
    .unwrap_err();
    assert_eq!(theirs, unknown);
    assert_eq!(theirs.code, Code::NotFound);

    let r = revoke(&store, &session(owner), &json!({"linkId": id}), now_ms())
        .await
        .unwrap();
    assert_eq!(r["revoked"], true);
    let again = revoke(&store, &session(owner), &json!({"linkId": id}), now_ms())
        .await
        .unwrap();
    assert_eq!(again["revoked"], true);
    let (reason, by): (String, String) =
        sqlx::query_as("SELECT revoked_reason, revoked_by FROM file_links WHERE link_id = ?")
            .bind(id)
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!((reason.as_str(), by), ("owner", owner.to_string()));
    let l = list(&store, &session(owner), now_ms()).await.unwrap();
    assert_eq!(l["links"][0]["state"], "revoked");
    assert_eq!(l["links"][0]["revokedReason"], "owner");
    assert_eq!(kinds(&store).await, vec!["link.created", "link.revoked"]);
    let subject: Option<String> = sqlx::query_scalar(
        "SELECT subject_principal_id FROM audit_events WHERE kind = 'link.revoked'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(subject, Some(owner.to_string()));
}

// ─── admin ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn admin_arms_follow_the_admin_rule() {
    let store = t1_store().await;
    let admin = PrincipalId::new_v7();
    let args = json!({});
    // Not an admin account.
    let e = admin_list(&store, &session(admin), false, &args, now_ms())
        .await
        .unwrap_err();
    assert_eq!(e.code, Code::Forbidden);
    // An admin on a view-tier device: not admin strength.
    let phone = device(admin, Tier::View, Tier::View.caps());
    let e = admin_list(&store, &phone, true, &args, now_ms())
        .await
        .unwrap_err();
    assert_eq!(e, AccessError::forbidden_admin());
    // An admin inside a share.
    let mut shared = session(admin);
    shared.share = Some(share_of(PrincipalId::new_v7()));
    let e = admin_revoke(&store, &shared, true, &json!({"linkId": "x"}), now_ms())
        .await
        .unwrap_err();
    assert_eq!(e.code, Code::Forbidden);
    let e = admin_revoke(
        &store,
        &session(admin),
        false,
        &json!({"linkId": "x"}),
        now_ms(),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, Code::Forbidden);
}

#[tokio::test]
async fn an_admin_lists_every_account_and_revokes_one_or_all() {
    let store = t1_store().await;
    let admin = PrincipalId::new_v7();
    let a = PrincipalId::new_v7();
    let b = PrincipalId::new_v7();
    sqlx::query("INSERT INTO accounts (principal_id, username) VALUES (?, 'ada'), (?, 'bo')")
        .bind(a.to_string())
        .bind(b.to_string())
        .execute(store.pool())
        .await
        .unwrap();
    let a1 = insert_raw(&store, a, None).await;
    let a2 = insert_raw(&store, a, None).await;
    let b1 = insert_raw(&store, b, None).await;
    let all = admin_list(&store, &session(admin), true, &json!({}), now_ms())
        .await
        .unwrap();
    assert_eq!(all["links"].as_array().unwrap().len(), 3);
    let only_a = admin_list(
        &store,
        &session(admin),
        true,
        &json!({"ownerPrincipalId": a.to_string()}),
        now_ms(),
    )
    .await
    .unwrap();
    let only_a = only_a["links"].as_array().unwrap();
    assert_eq!(only_a.len(), 2);
    assert!(only_a.iter().all(|l| l["ownerUsername"] == "ada"));
    assert!(admin_list(
        &store,
        &session(admin),
        true,
        &json!({"ownerPrincipalId": "nope"}),
        now_ms()
    )
    .await
    .is_err());

    let r = admin_revoke(
        &store,
        &session(admin),
        true,
        &json!({"linkId": b1}),
        now_ms(),
    )
    .await
    .unwrap();
    assert_eq!(r["revoked"], 1);
    let r = admin_revoke(
        &store,
        &session(admin),
        true,
        &json!({"ownerPrincipalId": a.to_string()}),
        now_ms(),
    )
    .await
    .unwrap();
    assert_eq!(r["revoked"], 2);
    let reasons: Vec<(String, String)> = sqlx::query_as(
        "SELECT link_id, revoked_reason FROM file_links WHERE link_id IN (?, ?, ?) ORDER BY link_id",
    )
    .bind(&a1)
    .bind(&a2)
    .bind(&b1)
    .fetch_all(store.pool())
    .await
    .unwrap();
    assert!(reasons.iter().all(|(_, r)| r == "admin"));
    assert_eq!(kinds(&store).await.len(), 3, "one link.revoked per link");
    // Live-only by default; includeEnded shows the revoked rows.
    let live = admin_list(&store, &session(admin), true, &json!({}), now_ms())
        .await
        .unwrap();
    assert_eq!(live["links"].as_array().unwrap().len(), 0);
    let ended = admin_list(
        &store,
        &session(admin),
        true,
        &json!({"includeEnded": true}),
        now_ms(),
    )
    .await
    .unwrap();
    assert_eq!(ended["links"].as_array().unwrap().len(), 3);
    // Revoking again is zero, an unknown id is not_found, both forms is invalid.
    let r = admin_revoke(
        &store,
        &session(admin),
        true,
        &json!({"linkId": b1}),
        now_ms(),
    )
    .await
    .unwrap();
    assert_eq!(r["revoked"], 0);
    let e = admin_revoke(
        &store,
        &session(admin),
        true,
        &json!({"linkId": PrincipalId::new_v7().to_string()}),
        now_ms(),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, Code::NotFound);
    let e = admin_revoke(
        &store,
        &session(admin),
        true,
        &json!({"linkId": b1, "ownerPrincipalId": a.to_string()}),
        now_ms(),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, Code::InvalidRequest);
}

/// On T0 every arm answers `not_found: outside links need a server`, and
/// status says links are off.
#[tokio::test]
async fn t0_has_no_outside_links() {
    let store = AccessStore::memory_t0().await;
    let reg = crate::access::sockets::Registry::new();
    let env = Env {
        tier: StoreTier::T0,
        store: Some(&store),
        pairing: None,
        sockets: &*reg,
        principal: crate::access::rpc::PrincipalInfo {
            username: "ned".into(),
            is_admin: false,
        },
        public_url: Some(BASE.into()),
        insecure_cookie: false,
    };
    let ctx = session(PrincipalId::new_v7());
    for cmd in [
        "access_file_link_create",
        "access_file_links_list",
        "access_file_link_revoke",
        "access_file_links_admin_list",
        "access_file_link_admin_revoke",
    ] {
        let e = crate::access::rpc::dispatch(&env, &ctx, cmd, &json!({"path": "/x"}))
            .await
            .unwrap_err();
        assert_eq!(e.code, Code::NotFound, "{cmd}");
        assert_eq!(e.message, NEEDS_SERVER, "{cmd}");
    }
    assert!(!enabled(StoreTier::T0));
}
