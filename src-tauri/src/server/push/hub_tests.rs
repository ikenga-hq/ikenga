//! Hub, store and RPC tests over real access stores (T0 in memory; T1 on a
//! temp operator root). The push service is a recording fake.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine as _;
use ring::agreement;
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{json, Value};

use super::hub::{HubConfig, PushHub};
use super::sender::{Backoff, BoxFuture, EndpointPolicy, PushRequest, PushResponse, PushTransport};
use super::store::{self, NewSub, SubVia};
use super::vapid::Vapid;
use super::{PushEvent, PushKind, PAYLOAD_LEN};
use crate::access::caps::{CapSet, Tier};
use crate::access::ctx::{AccessCtx, RequestMeta, Via};
use crate::access::devices::tests::{operator_ctx, pair};
use crate::access::rpc::{Env, PrincipalInfo};
use crate::access::sockets::Registry;
use crate::access::store::{AccessStore, StoreTier};
use crate::access::Code;
use crate::executor::PrincipalId;

/// Records every request; answers `status` (default 201).
#[derive(Default)]
struct Fake {
    sent: Mutex<Vec<PushRequest>>,
    status: Mutex<Option<u16>>,
}

impl PushTransport for Fake {
    fn post<'a>(&'a self, req: &'a PushRequest) -> BoxFuture<'a, Result<PushResponse, String>> {
        Box::pin(async move {
            self.sent.lock().unwrap().push(req.clone());
            Ok(PushResponse {
                status: self.status.lock().unwrap().unwrap_or(201),
                retry_after: None,
            })
        })
    }
}

fn hub(store: &AccessStore, fake: Arc<Fake>) -> Arc<PushHub> {
    PushHub::new(HubConfig {
        store: store.clone(),
        vapid: Some(Arc::new(
            Vapid::generate("mailto:t@example.com".into()).unwrap(),
        )),
        disabled_reason: None,
        policy: EndpointPolicy::default(),
        transport: fake,
        backoff: Backoff(vec![Duration::ZERO]),
    })
}

/// A browser subscription key pair: the private half kept for decryption.
struct Ua {
    private: Option<agreement::EphemeralPrivateKey>,
    public: Vec<u8>,
    auth: [u8; 16],
}

fn ua() -> Ua {
    let rng = SystemRandom::new();
    let private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
    let public = private.compute_public_key().unwrap().as_ref().to_vec();
    let mut auth = [0u8; 16];
    rng.fill(&mut auth).unwrap();
    Ua {
        private: Some(private),
        public,
        auth,
    }
}

fn sub(
    h: &PushHub,
    principal: &str,
    via: SubVia,
    device_id: Option<&str>,
    n: usize,
    ua: &Ua,
) -> NewSub {
    NewSub {
        principal_id: principal.into(),
        device_id: device_id.map(str::to_string),
        via,
        session_epoch: None,
        session_ref: None,
        endpoint: format!("https://fcm.googleapis.com/fcm/send/sub-{n}"),
        endpoint_origin: "https://fcm.googleapis.com".into(),
        p256dh: ua.public.clone(),
        auth: ua.auth.to_vec(),
        vapid_key_id: h.vapid().unwrap().key_id(),
        kinds: PushKind::SELECTABLE.to_vec(),
        label: Some(format!("phone {n}")),
        user_agent: None,
    }
}

async fn target_labels(h: &PushHub, p: PrincipalId, kind: PushKind) -> Vec<String> {
    let mut v: Vec<String> = h
        .targets(p, kind)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.sub.label.unwrap())
        .collect();
    v.sort();
    v
}

/// Each kind reaches only the credentials entitled to it (T0): permission
/// needs approve, pairing and update need admin strength, runs go to all.
#[tokio::test]
async fn t0_kinds_reach_only_entitled_credentials() {
    let store = AccessStore::memory_t0().await;
    let h = hub(&store, Arc::default());
    let owner = store.meta().owner_principal_id.unwrap();
    let o = owner.to_string();
    let host = store.meta().host_device_id.clone();
    let k = ua();
    let (approve, _) = pair(&store, Tier::Approve).await;
    let (dispatch, _) = pair(&store, Tier::Dispatch).await;
    let (full, _) = pair(&store, Tier::Full).await;
    let mut s_op = sub(&h, &o, SubVia::Operator, host.as_deref(), 0, &k);
    s_op.label = Some("operator".into());
    store::upsert(h.pool(), &s_op).await.unwrap();
    for (row, label, n) in [
        (&approve, "approve", 1),
        (&dispatch, "dispatch", 2),
        (&full, "full", 3),
    ] {
        let mut s = sub(&h, &o, SubVia::Device, Some(&row.device_id), n, &k);
        s.label = Some(label.into());
        store::upsert(h.pool(), &s).await.unwrap();
    }

    assert_eq!(
        target_labels(&h, owner, PushKind::Permission).await,
        vec!["approve", "full", "operator"]
    );
    assert_eq!(
        target_labels(&h, owner, PushKind::RunFinished).await,
        vec!["approve", "dispatch", "full", "operator"]
    );
    assert_eq!(
        target_labels(&h, owner, PushKind::Pairing).await,
        vec!["full", "operator"]
    );
    assert_eq!(
        target_labels(&h, owner, PushKind::Update).await,
        vec!["full", "operator"]
    );
    // Another principal's events never reach these rows.
    assert!(
        target_labels(&h, PrincipalId::new_v7(), PushKind::RunFinished)
            .await
            .is_empty()
    );

    // Routing "this device only" (the approve phone): permission goes there alone.
    sqlx::query("INSERT INTO routing_prefs (principal_id, mode, device_id, updated_at) VALUES (?, 'this_device', ?, 0)")
        .bind(&o)
        .bind(&approve.device_id)
        .execute(store.pool())
        .await
        .unwrap();
    assert_eq!(
        target_labels(&h, owner, PushKind::Permission).await,
        vec!["approve"]
    );

    // A subscription that opted out of a kind doesn't get it.
    let approve_sub = store::list(h.pool(), &o)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.label.as_deref() == Some("approve"))
        .unwrap();
    store::set_kinds(h.pool(), &o, &approve_sub.sub_id, &[PushKind::RunFailed])
        .await
        .unwrap();
    assert!(target_labels(&h, owner, PushKind::Permission)
        .await
        .is_empty());

    // A tier change is honoured at send time (approve → view).
    sqlx::query("UPDATE devices SET tier = 'dispatch' WHERE device_id = ?")
        .bind(&full.device_id)
        .execute(store.pool())
        .await
        .unwrap();
    assert_eq!(
        target_labels(&h, owner, PushKind::Pairing).await,
        vec!["operator"]
    );

    // A subscription under an old VAPID key is dead.
    sqlx::query("UPDATE push_subscriptions SET vapid_key_id = 'old'")
        .execute(store.pool())
        .await
        .unwrap();
    assert!(target_labels(&h, owner, PushKind::RunFinished)
        .await
        .is_empty());
}

/// Revoking a device grant removes its subscriptions in the same
/// transaction; other devices keep theirs.
#[tokio::test]
async fn revoking_a_device_removes_its_subscriptions() {
    let store = AccessStore::memory_t0().await;
    let h = hub(&store, Arc::default());
    let o = store.meta().owner_principal_id.unwrap().to_string();
    let k = ua();
    let (a, _) = pair(&store, Tier::Approve).await;
    let (b, _) = pair(&store, Tier::Approve).await;
    store::upsert(
        h.pool(),
        &sub(&h, &o, SubVia::Device, Some(&a.device_id), 1, &k),
    )
    .await
    .unwrap();
    store::upsert(
        h.pool(),
        &sub(&h, &o, SubVia::Device, Some(&b.device_id), 2, &k),
    )
    .await
    .unwrap();
    crate::access::devices::revoke(&store, &operator_ctx(&store), &a.device_id)
        .await
        .unwrap();
    let left = store::list(h.pool(), &o).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].device_id.as_deref(), Some(b.device_id.as_str()));
}

/// The real send path: encrypted for the subscription (decrypted here with
/// its private key), the fixed-size minimal payload, the headers, and a
/// VAPID JWT for the endpoint's origin.
#[tokio::test]
async fn deliver_encrypts_a_minimal_payload_with_headers() {
    let store = AccessStore::memory_t0().await;
    let fake = Arc::new(Fake::default());
    let h = hub(&store, fake.clone());
    let owner = store.meta().owner_principal_id.unwrap();
    let host = store.meta().host_device_id.clone();
    let mut k = ua();
    store::upsert(
        h.pool(),
        &sub(
            &h,
            &owner.to_string(),
            SubVia::Operator,
            host.as_deref(),
            1,
            &k,
        ),
    )
    .await
    .unwrap();

    let mut ev = PushEvent::new(None, PushKind::Permission, "n:77");
    ev.ttl = Some(90);
    let out = h.deliver(&ev, None).await;
    assert_eq!(out.len(), 1);
    let req = fake.sent.lock().unwrap()[0].clone();
    assert_eq!(
        (req.ttl, req.urgency, req.topic),
        (90, "high", "permission")
    );
    assert!(req.authorization.starts_with("vapid t="));
    let jwt = req.authorization["vapid t=".len()..]
        .split(',')
        .next()
        .unwrap();
    let claims: Value =
        serde_json::from_slice(&B64.decode(jwt.split('.').nth(1).unwrap()).unwrap()).unwrap();
    assert_eq!(claims["aud"], "https://fcm.googleapis.com");

    let as_public = &req.body[21..86];
    let ecdh = agreement::agree_ephemeral(
        k.private.take().unwrap(),
        &agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, as_public),
        |s| s.to_vec(),
    )
    .unwrap();
    let plain = super::crypto::decrypt_with(&ecdh, &k.auth, &k.public, &req.body).unwrap();
    assert_eq!(plain, br#"{"v":1,"k":"permission","r":"n:77"}"#);
    assert_eq!(req.body.len(), 86 + PAYLOAD_LEN + 16);

    // Success is recorded.
    let row = store::list(h.pool(), &owner.to_string()).await.unwrap();
    assert!(row[0].last_success_at.is_some());
}

/// 404 / 410 delete the subscription; a 403 counts a failure; enough
/// failures with no success in 30 days are pruned.
#[tokio::test]
async fn gone_endpoints_are_pruned() {
    let store = AccessStore::memory_t0().await;
    let fake = Arc::new(Fake::default());
    let h = hub(&store, fake.clone());
    let owner = store.meta().owner_principal_id.unwrap();
    let o = owner.to_string();
    let host = store.meta().host_device_id.clone();
    let k = ua();
    store::upsert(
        h.pool(),
        &sub(&h, &o, SubVia::Operator, host.as_deref(), 1, &k),
    )
    .await
    .unwrap();
    let ev = PushEvent::new(None, PushKind::RunFinished, "run:a");

    *fake.status.lock().unwrap() = Some(403);
    h.deliver(&ev, None).await;
    assert_eq!(store::list(h.pool(), &o).await.unwrap()[0].failure_count, 1);

    for status in [410, 404] {
        store::upsert(
            h.pool(),
            &sub(&h, &o, SubVia::Operator, host.as_deref(), 1, &k),
        )
        .await
        .unwrap();
        *fake.status.lock().unwrap() = Some(status);
        h.deliver(&ev, None).await;
        assert!(
            store::list(h.pool(), &o).await.unwrap().is_empty(),
            "{status}"
        );
    }

    store::upsert(
        h.pool(),
        &sub(&h, &o, SubVia::Operator, host.as_deref(), 2, &k),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE push_subscriptions SET failure_count = 10, created_at = 0")
        .execute(h.pool())
        .await
        .unwrap();
    assert_eq!(store::prune(h.pool(), store::now_ms()).await.unwrap(), 1);
}

#[tokio::test]
async fn the_per_principal_cap_evicts_the_oldest() {
    let store = AccessStore::memory_t0().await;
    let h = hub(&store, Arc::default());
    let o = store.meta().owner_principal_id.unwrap().to_string();
    let host = store.meta().host_device_id.clone();
    let k = ua();
    for n in 0..(store::MAX_PER_PRINCIPAL as usize + 3) {
        store::upsert(
            h.pool(),
            &sub(&h, &o, SubVia::Operator, host.as_deref(), n, &k),
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let rows = store::list(h.pool(), &o).await.unwrap();
    assert_eq!(rows.len() as i64, store::MAX_PER_PRINCIPAL);
    assert!(!rows.iter().any(|r| r.label.as_deref() == Some("phone 0")));
}

fn env<'a>(store: &'a AccessStore, sockets: &'a Registry) -> Env<'a> {
    Env {
        tier: StoreTier::T0,
        store: Some(store),
        pairing: None,
        sockets,
        principal: PrincipalInfo {
            username: "ned".into(),
            is_admin: false,
        },
        public_url: None,
        insecure_cookie: false,
    }
}

fn device_ctx(store: &AccessStore, device_id: &str, tier: Tier) -> AccessCtx {
    AccessCtx {
        principal_id: store.meta().owner_principal_id.unwrap(),
        via: Via::Device {
            device_id: device_id.into(),
        },
        device_id: Some(device_id.into()),
        tier,
        share: None,
        share_headers: false,
        caps: tier.caps(),
        admin_strength: tier == Tier::Full,
        meta: RequestMeta::default(),
    }
}

/// The arms: config advertises only entitled kinds; subscribe binds the
/// credential; list never returns keys or the endpoint path; update,
/// test (throttled) and unsubscribe; a child token can't subscribe.
#[tokio::test]
async fn rpc_arms_bind_the_caller_and_never_return_secrets() {
    use super::rpc::dispatch_with;
    let store = AccessStore::memory_t0().await;
    let fake = Arc::new(Fake::default());
    let h = hub(&store, fake.clone());
    let reg = Registry::new();
    let e = env(&store, &reg);
    let (phone, _) = pair(&store, Tier::Dispatch).await;
    let ctx = device_ctx(&store, &phone.device_id, Tier::Dispatch);

    let cfg = dispatch_with(Some(&*h), &e, &ctx, "access_push_config", &json!({}))
        .await
        .unwrap();
    assert_eq!(cfg["enabled"], true);
    assert_eq!(cfg["keyId"], h.vapid().unwrap().key_id());
    let kinds: Vec<&str> = cfg["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        vec!["run_finished", "run_failed", "run_cancelled", "invite"]
    );
    let op = dispatch_with(
        Some(&*h),
        &e,
        &operator_ctx(&store),
        "access_push_config",
        &json!({}),
    )
    .await
    .unwrap();
    assert_eq!(op["kinds"].as_array().unwrap().len(), 7);

    let k = ua();
    let endpoint = "https://fcm.googleapis.com/fcm/send/SECRET-PATH";
    let subscribe = json!({
        "endpoint": endpoint,
        "keys": {"p256dh": B64.encode(&k.public), "auth": B64.encode(k.auth)},
        "kinds": ["permission", "run_failed"],
        "label": "Chrome on\u{202e} Android",
    });
    let r = dispatch_with(Some(&*h), &e, &ctx, "access_push_subscribe", &subscribe)
        .await
        .unwrap();
    let sub_id = r["subId"].as_str().unwrap().to_string();
    // `permission` was narrowed away (a dispatch device can't approve).
    let row = store::get(h.pool(), &sub_id).await.unwrap().unwrap();
    assert_eq!(row.kinds, vec![PushKind::RunFailed]);
    assert_eq!(row.device_id.as_deref(), Some(phone.device_id.as_str()));
    assert_eq!(row.label.as_deref(), Some("Chrome on Android"));

    let list = dispatch_with(Some(&*h), &e, &ctx, "access_push_list", &json!({}))
        .await
        .unwrap();
    let text = list.to_string();
    assert!(!text.contains("SECRET-PATH"));
    assert!(!text.contains(&B64.encode(&k.public)));
    assert!(!text.contains(&B64.encode(k.auth)));
    assert_eq!(list[0]["endpointHost"], "fcm.googleapis.com");
    assert_eq!(list[0]["thisDevice"], true);

    let upd = dispatch_with(
        Some(&*h),
        &e,
        &ctx,
        "access_push_update",
        &json!({"subId": sub_id, "kinds": ["run_finished", "pairing"]}),
    )
    .await
    .unwrap();
    assert_eq!(upd["kinds"], json!(["run_finished"]));

    let t = dispatch_with(
        Some(&*h),
        &e,
        &ctx,
        "access_push_test",
        &json!({"subId": sub_id}),
    )
    .await
    .unwrap();
    assert_eq!(t["queued"], true);
    let again = dispatch_with(
        Some(&*h),
        &e,
        &ctx,
        "access_push_test",
        &json!({"subId": sub_id}),
    )
    .await
    .unwrap_err();
    assert_eq!(again.code, Code::Throttled);

    // Bad input is refused before anything is stored.
    for bad in [
        json!({"endpoint": "https://evil.example/x", "keys": subscribe["keys"].clone()}),
        json!({"endpoint": endpoint, "keys": {"p256dh": "AAAA", "auth": B64.encode(k.auth)}}),
        json!({"endpoint": endpoint, "keys": {"p256dh": B64.encode(&k.public), "auth": "AA"}}),
    ] {
        let err = dispatch_with(Some(&*h), &e, &ctx, "access_push_subscribe", &bad)
            .await
            .unwrap_err();
        assert_eq!(err.code, Code::InvalidRequest, "{bad}");
    }

    // A broker → child context can't touch subscriptions.
    let mut child = ctx.clone();
    child.via = Via::ChildToken;
    child.caps = CapSet::ALL;
    assert_eq!(
        dispatch_with(Some(&*h), &e, &child, "access_push_list", &json!({}))
            .await
            .unwrap_err()
            .code,
        Code::Forbidden
    );

    let rm = dispatch_with(
        Some(&*h),
        &e,
        &ctx,
        "access_push_unsubscribe",
        &json!({"subId": sub_id}),
    )
    .await
    .unwrap();
    assert_eq!(rm["removed"], 1);

    // No hub: config says off, list is empty.
    let off = dispatch_with(None, &e, &ctx, "access_push_config", &json!({}))
        .await
        .unwrap();
    assert_eq!(off["enabled"], false);
}

/// T1: `update` reaches only enabled admins, once per version; a session
/// subscription dies with its session epoch; a disabled account gets
/// nothing, and disabling deletes its subscriptions.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn t1_update_goes_to_admins_only_and_sessions_are_epoch_bound() {
    use crate::server::operator::{open_accounts, test_support::temp_root, Opener};
    let (_tmp, root) = temp_root();
    let pool = open_accounts(&root, Opener::Broker).await.unwrap();
    let store = AccessStore::attach_t1(pool.clone()).await.unwrap();
    let fake = Arc::new(Fake::default());
    let h = hub(&store, fake.clone());
    let admin = PrincipalId::new_v7();
    let member = PrincipalId::new_v7();
    for (id, name, uid, is_admin) in [(admin, "ada", 20001, 1), (member, "bo", 20002, 0)] {
        sqlx::query(
            "INSERT INTO accounts (principal_id, username, unix_name, unix_uid, unix_gid, home, \
             shell, is_admin, session_epoch, adopted, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, '/h', '/bin/sh', ?, 3, 0, 0, 0)",
        )
        .bind(id.to_string())
        .bind(name)
        .bind(format!("ik-{name}"))
        .bind(uid)
        .bind(uid)
        .bind(is_admin)
        .execute(&pool)
        .await
        .unwrap();
    }
    let k = ua();
    for (id, n) in [(admin, 1), (member, 2)] {
        let mut s = sub(&h, &id.to_string(), SubVia::Session, None, n, &k);
        s.session_epoch = Some(3);
        s.session_ref = Some(store::session_ref("sess"));
        store::upsert(h.pool(), &s).await.unwrap();
    }

    assert_eq!(h.admins().await.unwrap(), vec![admin]);
    assert_eq!(
        target_labels(&h, admin, PushKind::Update).await,
        vec!["phone 1"]
    );
    assert!(target_labels(&h, member, PushKind::Update).await.is_empty());
    assert_eq!(
        target_labels(&h, member, PushKind::RunFinished).await,
        vec!["phone 2"]
    );
    // A session subscription can't receive permission asks routed elsewhere,
    // but does with the default routing.
    assert_eq!(
        target_labels(&h, member, PushKind::Permission).await,
        vec!["phone 2"]
    );

    assert_eq!(h.announce_update("0.21.0").await, vec![admin]);
    assert!(
        h.announce_update("0.21.0").await.is_empty(),
        "once per version"
    );

    // A forced logout bumps the epoch: the session subscription is dead.
    sqlx::query("UPDATE accounts SET session_epoch = 4 WHERE principal_id = ?")
        .bind(member.to_string())
        .execute(&pool)
        .await
        .unwrap();
    assert!(target_labels(&h, member, PushKind::RunFinished)
        .await
        .is_empty());

    // Disabled admin: nothing at send time, and the delete hook clears rows.
    sqlx::query("UPDATE accounts SET disabled_at = 1 WHERE principal_id = ?")
        .bind(admin.to_string())
        .execute(&pool)
        .await
        .unwrap();
    assert!(target_labels(&h, admin, PushKind::Update).await.is_empty());
    assert!(h.admins().await.unwrap().is_empty());
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        store::delete_for_principal(&mut conn, &admin.to_string())
            .await
            .unwrap(),
        1
    );
    // Logout removes the session's own subscription.
    assert_eq!(
        store::delete_for_session(&mut conn, "sess").await.unwrap(),
        1
    );
}

/// The update producer hook: a registered source is polled at once and its
/// version announced to the admins (T0: the owner) — without the version in
/// the push.
#[tokio::test]
async fn an_update_source_announces_to_the_owner() {
    struct Source;
    impl super::hub::UpdateSource for Source {
        fn available_version(&self) -> Option<String> {
            Some("9.9.9".into())
        }
    }
    let store = AccessStore::memory_t0().await;
    let fake = Arc::new(Fake::default());
    let h = hub(&store, fake.clone());
    let owner = store.meta().owner_principal_id.unwrap();
    let host = store.meta().host_device_id.clone();
    let k = ua();
    store::upsert(
        h.pool(),
        &sub(
            &h,
            &owner.to_string(),
            SubVia::Operator,
            host.as_deref(),
            1,
            &k,
        ),
    )
    .await
    .unwrap();
    h.set_update_source(Arc::new(Source));
    for _ in 0..100 {
        if !fake.sent.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let sent = fake.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!((sent[0].topic, sent[0].urgency), ("update", "low"));
    assert_eq!(
        store::meta_get(h.pool(), "last_update_version")
            .await
            .unwrap()
            .as_deref(),
        Some("9.9.9")
    );
}
