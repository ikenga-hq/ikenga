//! `/ws/events` under T1: principal A's socket never carries principal B's
//! events. The launcher here serves a **real daemon router** per principal
//! (its own data dir, home and event bus, in-process), so the whole path —
//! broker cookie → narrowing → proxy → the child's auth → its bus — is the
//! production one; only the setuid launch is faked (the root tests in
//! `server/tests/broker_t1.rs` cover that).

use std::collections::HashMap;
use std::path::PathBuf;

use axum::http::{HeaderName, HeaderValue};

use super::*;
use crate::access::{AccessOptions, CapSet, DaemonAccess};
use crate::engines::EngineRegistry;
use crate::executor::ExecutorTier;
use crate::pty::PtyManager;
use crate::server::broker::proxy::{Narrower, Narrowing, Refusal, CAPS_HEADER};
use crate::server::ServerConfig;

/// One real principal-child router per launch, rooted under `root/<id>/`.
struct DaemonLauncher {
    root: PathBuf,
    homes: Mutex<HashMap<PrincipalId, PathBuf>>,
}

impl ChildLauncher for DaemonLauncher {
    fn launch<'a>(
        &'a self,
        principal: &'a Principal,
        token: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<LaunchedChild>> {
        Box::pin(async move {
            let base = self.root.join(principal.id.to_string());
            let (data, home) = (base.join("data"), base.join("home"));
            for d in [&data, &home] {
                std::fs::create_dir_all(d)?;
            }
            self.homes
                .lock()
                .unwrap()
                .insert(principal.id, home.clone());
            let config = ServerConfig {
                host: "127.0.0.1".into(),
                port: 0,
                static_dir: PathBuf::from("no-spa-here"),
                pkgs_dir: None,
                data_dir: Some(data.clone()),
                // The per-child bearer the broker presents upstream.
                auth_token: Some(token.to_string()),
                allowed_origins: vec![],
                idle_timeout_secs: None,
                executor_tier: ExecutorTier::T0,
            };
            let router = crate::server::build_router(
                config,
                Arc::new(PtyManager::new()),
                Arc::new(EngineRegistry::new()),
                Some(Arc::new(crate::db::PaDb::new(data.join("ikenga.db")))),
                None,
                Some(home),
                crate::server::rpc_shell::PathGuard::allowlist(),
                None,
                DaemonAccess::principal_child(AccessOptions::default()),
                None,
                crate::server::UpdateSource::Default,
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let addr = listener.local_addr()?;
            tokio::spawn(async move {
                let _ = axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await;
            });
            Ok(LaunchedChild {
                addr,
                process: Box::new(FakeProcess::default()),
            })
        })
    }
}

/// Every proxied request carries every cap (the access layer's narrower is
/// not what this test is about; without one a child grants nothing).
struct AllCaps;

impl Narrower for AllCaps {
    fn narrow<'a>(
        &'a self,
        _ctx: &'a crate::server::auth::PrincipalCtx,
        _req: &'a axum::http::request::Parts,
    ) -> BoxFuture<'a, Result<Narrowing, Refusal>> {
        Box::pin(async {
            Ok(Narrowing {
                headers: vec![(
                    HeaderName::from_static(CAPS_HEADER),
                    HeaderValue::from_str(&CapSet::ALL.to_header()).unwrap(),
                )],
                ..Narrowing::default()
            })
        })
    }
}

struct Events {
    _tmp: tempfile::TempDir,
    pool: SqlitePool,
    launcher: Arc<DaemonLauncher>,
    app: Router,
}

async fn harness() -> Events {
    let (tmp, root) = test_support::temp_root();
    let pool = open_accounts(&root, Opener::Broker).await.unwrap();
    let launcher = Arc::new(DaemonLauncher {
        root: tmp.path().join("children"),
        homes: Mutex::new(HashMap::new()),
    });
    let verifier = Arc::new(
        tokio::task::spawn_blocking(LoginVerifier::new)
            .await
            .unwrap(),
    );
    let mut state = BrokerState::new(pool.clone(), verifier, launcher.clone()).unwrap();
    state.hooks.narrower = Arc::new(AllCaps);
    let store = backend::open_session_store(&root).await.unwrap();
    let app = router(
        Arc::new(state),
        store,
        &tmp.path().join("no-spa"),
        vec![],
        false,
        BrokerExtensions::default(),
    );
    Events {
        _tmp: tmp,
        pool,
        launcher,
        app,
    }
}

async fn next_text(ws: &mut Client, within: Duration) -> Option<Value> {
    tokio::time::timeout(within, async {
        while let Some(Ok(msg)) = ws.next().await {
            if let tungstenite::Message::Text(t) = msg {
                return Some(serde_json::from_str(&t).unwrap());
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

async fn open_events(addr: SocketAddr, cookie: &str) -> Client {
    let mut ws = ws_connect(addr, "/ws/events", cookie).await.unwrap();
    let ready = next_text(&mut ws, Duration::from_secs(10))
        .await
        .expect("a ready frame through the proxy");
    assert_eq!(ready["type"], "ready", "{ready}");
    assert!(
        ready["events"]
            .as_array()
            .unwrap()
            .contains(&json!("settings://changed")),
        "{ready}"
    );
    ws.send(tungstenite::Message::Text(
        json!({ "type": "subscribe", "events": ["settings://changed", "projects:active-changed"] })
            .to_string(),
    ))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    ws
}

async fn write_theme(app: &Router, cookie: &str, theme: &str) {
    let (status, body) = rpc(
        app,
        Some(cookie),
        json!({ "cmd": "settings_write_field", "args": {
            "scope": "personal", "field": "appearance.theme", "value": theme,
            "remove": false, "projectId": null
        }}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true, "{body}");
}

/// A principal's events reach that principal's sockets only. Each principal
/// writes its own settings; each socket sees exactly its own principal's
/// `settings://changed` (with its own home's path) and nothing from the other.
#[tokio::test]
async fn a_second_principal_never_receives_the_first_principals_events() {
    let h = harness().await;
    let ada = insert_account(&h.pool, "ada", 20_001, false).await;
    let bob = insert_account(&h.pool, "bob", 20_002, false).await;
    let a = login_cookie(&h.app, "ada").await;
    let b = login_cookie(&h.app, "bob").await;
    let addr = serve(h.app.clone()).await;

    let mut ws_a = open_events(addr, &a).await;
    let mut ws_b = open_events(addr, &b).await;
    let home = |id| h.launcher.homes.lock().unwrap().get(&id).cloned().unwrap();
    let settings_path = |id| {
        home(id)
            .join(".ikenga/settings.json")
            .to_string_lossy()
            .into_owned()
    };

    write_theme(&h.app, &a, "B").await;
    let got = next_text(&mut ws_a, Duration::from_secs(5))
        .await
        .expect("ada hears her own write");
    assert_eq!(got["event"], "settings://changed", "{got}");
    assert_eq!(got["payload"]["path"], settings_path(ada));
    assert!(
        next_text(&mut ws_b, Duration::from_millis(700))
            .await
            .is_none(),
        "bob must not receive ada's event"
    );

    write_theme(&h.app, &b, "C").await;
    let got = next_text(&mut ws_b, Duration::from_secs(5))
        .await
        .expect("bob hears his own write");
    assert_eq!(got["payload"]["path"], settings_path(bob));
    assert!(
        next_text(&mut ws_a, Duration::from_millis(700))
            .await
            .is_none(),
        "ada must not receive bob's event"
    );
    assert_ne!(settings_path(ada), settings_path(bob));
}

/// No session: the broker refuses the handshake before any child is reached.
#[tokio::test]
async fn the_events_socket_needs_a_session() {
    let h = harness().await;
    insert_account(&h.pool, "ada", 20_001, false).await;
    let addr = serve(h.app.clone()).await;
    assert!(ws_connect(addr, "/ws/events", "ikenga_session=forged")
        .await
        .is_err());
    assert!(
        h.launcher.homes.lock().unwrap().is_empty(),
        "no child launched"
    );
}

/// Defence in depth in the child: a relayed request carrying share headers
/// (the broker's narrowing for a share, which routes into the Owner's
/// child) is refused, so a share member can never subscribe to the Owner's
/// events. Both a parsed share (refused by the route's `owner` class) and
/// share headers that do not parse (refused by the handler) are covered.
#[tokio::test]
async fn a_child_refuses_an_events_socket_under_a_share() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let router = crate::server::build_router(
        ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            static_dir: PathBuf::from("no-spa-here"),
            pkgs_dir: None,
            data_dir: Some(data),
            auth_token: Some("child-token".into()),
            allowed_origins: vec![],
            idle_timeout_secs: None,
            executor_tier: ExecutorTier::T0,
        },
        Arc::new(PtyManager::new()),
        Arc::new(EngineRegistry::new()),
        None,
        None,
        None,
        crate::server::rpc_shell::PathGuard::allowlist(),
        None,
        DaemonAccess::principal_child(AccessOptions::default()),
        None,
        crate::server::UpdateSource::Default,
    );
    let addr = serve(router).await;
    let open = |share: Option<&'static str>| async move {
        use tungstenite::client::IntoClientRequest;
        let mut req = format!("ws://{addr}/ws/events")
            .into_client_request()
            .unwrap();
        let h = req.headers_mut();
        h.insert("authorization", "Bearer child-token".parse().unwrap());
        h.insert("x-ikenga-caps", CapSet::ALL.to_header().parse().unwrap());
        if let Some(name) = share {
            h.insert(name, "proj".parse().unwrap());
        }
        tokio_tungstenite::connect_async(req)
            .await
            .map(|(ws, _)| ws)
    };
    for header in ["x-ikenga-share-project", "x-ikenga-share-role"] {
        match open(Some(header)).await {
            Err(tungstenite::Error::Http(resp)) => {
                assert_eq!(resp.status().as_u16(), 403, "{header}")
            }
            other => panic!(
                "{header}: expected a 403 handshake, got {:?}",
                other.map(|_| ())
            ),
        }
    }
    // The control: the same child, no share header, upgrades.
    assert!(open(None).await.is_ok());
}
