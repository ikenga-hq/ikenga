//! The `seats_*` arms over the router (house pattern: a literal
//! `ServerConfig` → `oneshot` POST `/api/rpc` with the bearer token). The
//! stores' own behaviour is tested with the cores (`iyke/seats.rs`); these
//! pin what the daemon adds: argument shapes, the typed `error_data`, the
//! daemon's world, the engine calls through `chi_exec`, the queue poller and
//! per-principal isolation.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::StatusCode;
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::db::PaDb;
use crate::executor::ExecutorTier;
use crate::server::rpc_local::DaemonChi;
use crate::server::shared::chi_exec::{self, EngineResolver};
use crate::server::ServerConfig;

fn config(data_dir: Option<PathBuf>) -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".into(),
        port: 0,
        static_dir: PathBuf::from("no-spa-here"),
        pkgs_dir: None,
        data_dir,
        auth_token: Some("tok".into()),
        allowed_origins: vec![],
        idle_timeout_secs: None,
        executor_tier: ExecutorTier::T0,
    }
}

/// Finds no engine at all.
struct NoEngines;

impl EngineResolver for NoEngines {
    fn native(&self, _binary: &str) -> Option<PathBuf> {
        None
    }
    fn in_wsl<'a>(
        &'a self,
        _binary: &'a str,
        _distro: Option<&'a str>,
    ) -> futures_util::future::BoxFuture<'a, crate::server::shared::agents::WslLookup> {
        Box::pin(std::future::ready(crate::server::shared::agents::WslLookup::NotFound))
    }
}

/// One daemon: its own `--data-dir` (so its own `ikenga.db` — a T1
/// principal child's shape), a home, and an engine resolver.
struct Daemon {
    _tmp: tempfile::TempDir,
    db: Arc<PaDb>,
    router: Router,
}

fn daemon_with(resolver: Arc<dyn EngineResolver>) -> Daemon {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (data, home) = (root.join("data"), root.join("home"));
    for d in [&data, &home] {
        std::fs::create_dir_all(d).unwrap();
    }
    let db = Arc::new(PaDb::new(data.join("ikenga.db")));
    let chi = Arc::new(DaemonChi {
        runtime: Arc::new(chi_exec::ChiRuntime::new()),
        resolver,
    });
    let router =
        crate::server::router_with_chi(config(Some(data)), Some(db.clone()), Some(home), chi);
    Daemon {
        _tmp: tmp,
        db,
        router,
    }
}

/// `claude` resolves to the chi stub engine (unix), nothing else does.
fn daemon() -> Daemon {
    #[cfg(unix)]
    let resolver: Arc<dyn EngineResolver> =
        Arc::new(chi_exec::tests::StubResolver(chi_exec::tests::stub_claude()));
    #[cfg(not(unix))]
    let resolver: Arc<dyn EngineResolver> = Arc::new(NoEngines);
    daemon_with(resolver)
}

async fn rpc(router: &Router, cmd: &str, args: Value) -> Value {
    let res = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/rpc")
                .header("authorization", "Bearer tok")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "cmd": cmd, "args": args }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn ok(router: &Router, cmd: &str, args: Value) -> Value {
    let res = rpc(router, cmd, args.clone()).await;
    assert_eq!(res["ok"], true, "{cmd} {args} → {res}");
    res.get("data").cloned().unwrap_or(Value::Null)
}

/// A typed refusal: the whole response, asserting `error_data` is the
/// serialized `SeatError` and `error` is `"<cmd>: <code>: <message>"`.
async fn refused(router: &Router, cmd: &str, args: Value, code: &str) -> Value {
    let res = rpc(router, cmd, args.clone()).await;
    assert_eq!(res["ok"], false, "{cmd} {args} should fail → {res}");
    let data = &res["error_data"];
    assert_eq!(data["code"], code, "{cmd} {args} → {res}");
    let message = data["message"].as_str().unwrap();
    assert_eq!(
        res["error"].as_str().unwrap(),
        format!("{cmd}: {code}: {message}")
    );
    res
}

/// A plain (untyped) error string.
async fn plain_err(router: &Router, cmd: &str, args: Value) -> String {
    let res = rpc(router, cmd, args.clone()).await;
    assert_eq!(res["ok"], false, "{cmd} {args} should fail → {res}");
    assert!(res.get("error_data").is_none(), "{res}");
    res["error"].as_str().unwrap().to_string()
}

fn ui() -> Value {
    json!({ "client": "ui" })
}

async fn create(router: &Router, name: &str, engine: &str) -> Value {
    let r = ok(
        router,
        "seats_create",
        json!({
            "req": { "projectId": "default", "name": name, "engineId": engine, "start": { "kind": "empty" } },
            "actor": ui(),
        }),
    )
    .await;
    r["seat"].clone()
}

async fn list(router: &Router) -> Vec<Value> {
    ok(router, "seats_list", json!({ "projectId": "default" }))
        .await
        .as_array()
        .unwrap()
        .clone()
}

// ── reads ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn engines_report_this_servers_truth() {
    let d = daemon_with(Arc::new(NoEngines));
    let r = &d.router;
    let engines = ok(r, "seats_engines", json!({})).await;
    let get = |id: &str| {
        engines
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["engine_id"] == id)
            .unwrap()
            .clone()
    };
    for e in engines.as_array().unwrap() {
        // No terminal can be seated here: no wrap to spawn one.
        assert_eq!(e["wrap_id"], Value::Null, "{e}");
    }
    let claude = get("claude-code");
    assert_eq!(claude["seatable"], false);
    assert_eq!(claude["reason"], NOT_INSTALLED_HERE);
    let openrouter = get("openrouter");
    assert_eq!(openrouter["seatable"], false);
    assert_eq!(openrouter["reason"], chi_exec::HEADLESS_OPENROUTER);
    // A static refusal keeps its own reason.
    assert_eq!(get("gemini")["reason"], "not yet supported by iyke chi");

    // Found on PATH → seatable.
    #[cfg(unix)]
    {
        let d = daemon();
        let engines = ok(&d.router, "seats_engines", json!({})).await;
        let claude = engines
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["engine_id"] == "claude-code")
            .unwrap()
            .clone();
        assert_eq!(claude["seatable"], true);
        assert!(claude.get("reason").is_none(), "{claude}");
        assert_eq!(claude["engine_resume"], "durable");
    }
}

#[tokio::test]
async fn seats_need_a_data_dir() {
    let r = crate::server::router_with_chi(config(None), None, None, Arc::new(DaemonChi::host()));
    for cmd in [
        "seats_list",
        "seats_get",
        "seats_create",
        "seats_clear",
        "seats_resume",
    ] {
        let args = json!({
            "seat": { "seatId": "s" }, "seatId": "s", "prompt": "p", "actor": ui(),
            "opts": { "fallback": "fresh" },
            "req": { "name": "a", "engineId": "claude-code", "start": { "kind": "empty" } },
        });
        let e = plain_err(&r, cmd, args).await;
        assert_eq!(e, format!("{cmd}: {}", crate::server::rpc::NO_DB));
    }
    // The engine table reads no database.
    assert!(ok(&r, "seats_engines", json!({})).await.is_array());
}

#[tokio::test]
async fn malformed_arguments_are_plain_errors() {
    let d = daemon();
    let r = &d.router;
    assert_eq!(
        plain_err(r, "seats_clear", json!({ "seatId": "s" })).await,
        "seats_clear: `actor` is required"
    );
    assert_eq!(
        plain_err(r, "seats_get", json!({})).await,
        "seats_get: `seat` is required"
    );
    let e = plain_err(
        r,
        "seats_rename",
        json!({ "seatId": 7, "name": "x", "actor": ui() }),
    )
    .await;
    assert!(e.starts_with("seats_rename: invalid `seatId`"), "{e}");
}

// ── the store CRUD, end to end ──────────────────────────────────────────

#[tokio::test]
async fn create_list_get_rename_clear_release_remove() {
    let d = daemon();
    let r = &d.router;
    assert_eq!(list(r).await, Vec::<Value>::new());

    let seat = create(r, "lead", "claude-code").await;
    let id = seat["id"].as_str().unwrap().to_string();
    assert_eq!(seat["address"], "seat:default/lead");
    assert_eq!(seat["status"], "vacant");
    assert_eq!(
        seat["resume"],
        json!({ "resumable": false, "reason": "no_session" })
    );

    // No projectId → the active project (default).
    let all = ok(r, "seats_list", json!({ "projectId": null })).await;
    assert_eq!(all.as_array().unwrap().len(), 1);
    assert_eq!(all[0]["id"], id.as_str());

    // By id, by every address form.
    for seat in [
        json!({ "seatId": id }),
        json!({ "address": "seat:default/lead" }),
        json!({ "address": "default/lead" }),
        json!({ "address": "@lead" }),
        json!({ "address": "lead" }),
    ] {
        let got = ok(r, "seats_get", json!({ "seat": seat })).await;
        assert_eq!(got["id"], id.as_str(), "{seat}");
    }

    // Typed refusals carry their code (and details) in `error_data`.
    refused(
        r,
        "seats_create",
        json!({
            "req": { "projectId": "default", "name": "lead", "engineId": "claude-code", "start": { "kind": "empty" } },
            "actor": ui(),
        }),
        "seat_name_taken",
    )
    .await;
    refused(
        r,
        "seats_create",
        json!({
            "req": { "projectId": "nope", "name": "x", "engineId": "claude-code", "start": { "kind": "empty" } },
            "actor": ui(),
        }),
        "project_not_found",
    )
    .await;
    refused(
        r,
        "seats_get",
        json!({ "seat": { "address": "bad name!" } }),
        "invalid_address",
    )
    .await;

    // Rename (snake_case args too).
    let renamed = ok(
        r,
        "seats_rename",
        json!({ "seat_id": id, "name": "boss", "actor": ui() }),
    )
    .await;
    assert_eq!(renamed["address"], "seat:default/boss");

    // A hold by another client refuses the UI, with the holder in details.
    ok(
        r,
        "seats_resolve",
        json!({ "seat": { "seatId": id }, "actor": { "client": "cli", "hold": true } }),
    )
    .await;
    let held = refused(
        r,
        "seats_clear",
        json!({ "seatId": id, "actor": ui() }),
        "seat_held",
    )
    .await;
    assert_eq!(held["error_data"]["details"]["client"], "cli");
    // The holder releases its own hold; then Clear goes through.
    let released = ok(
        r,
        "seats_release",
        json!({ "seatId": id, "actor": { "client": "cli" } }),
    )
    .await;
    assert_eq!(released["hold"], Value::Null);
    let cleared = ok(r, "seats_clear", json!({ "seatId": id, "actor": ui() })).await;
    assert_eq!(cleared["session"], Value::Null);

    let removed = ok(
        r,
        "seats_remove",
        json!({ "seatId": id, "opts": { "removeMemory": true }, "actor": ui() }),
    )
    .await;
    assert_eq!(removed, json!({ "seat_id": id }));
    assert_eq!(list(r).await, Vec::<Value>::new());
    refused(
        r,
        "seats_get",
        json!({ "seat": { "seatId": id } }),
        "seat_not_found",
    )
    .await;
}

#[tokio::test]
async fn openrouter_and_terminals_are_refused_with_the_reason() {
    let d = daemon();
    let r = &d.router;
    let e = refused(
        r,
        "seats_create",
        json!({
            "req": { "name": "or", "engineId": "openrouter", "start": { "kind": "empty" } },
            "actor": ui(),
        }),
        "engine_unsupported",
    )
    .await;
    assert_eq!(e["error_data"]["message"], chi_exec::HEADLESS_OPENROUTER);
    assert_eq!(e["error_data"]["details"]["engine_id"], "openrouter");

    let seat = create(r, "lead", "claude-code").await;
    let terminal = json!({ "kind": "terminal", "terminalId": "t1", "engineId": "claude-code" });
    let moved = refused(
        r,
        "seats_move",
        json!({ "session": terminal, "toSeatId": seat["id"], "actor": ui() }),
        "terminal_not_found",
    )
    .await;
    assert_eq!(moved["error_data"]["message"], NO_TERMINAL_SEATS);
    refused(
        r,
        "seats_create",
        json!({
            "req": { "name": "t", "engineId": "claude-code",
                     "start": { "kind": "session", "session": terminal } },
            "actor": ui(),
        }),
        "terminal_not_found",
    )
    .await;
    assert_eq!(list(r).await.len(), 1, "a refused create leaves no seat");

    // No path T here: a vacant claude seat gets no claim, so the UI's first
    // send is a headless run (path H).
    let route = ok(
        r,
        "seats_resolve",
        json!({ "seat": { "seatId": seat["id"] }, "actor": ui(), "opts": { "claimResume": true } }),
    )
    .await;
    assert_eq!(route["route"], "vacant");
    assert_eq!(route["claim"], Value::Null);
}

// ── engine calls (unix: the stub `claude`) ──────────────────────────────

#[cfg(unix)]
async fn run_status(r: &Router, run_id: &str) -> Value {
    ok(r, "chi_status", json!({ "runId": run_id })).await
}

#[cfg(unix)]
async fn wait_done(r: &Router, run_id: &str) {
    assert!(
        chi_exec::tests::wait_for(|| async { run_status(r, run_id).await["status"] == "done" })
            .await,
        "run {run_id} never finished"
    );
}

/// Path H end to end: a vacant seat resumes fresh through `chi_exec` (owner
/// `seat`), binds the run, reads `idle` once it is done, and the next resume
/// is refused as not vacant; a fill then replaces the run, reporting the
/// previous one.
#[cfg(unix)]
#[tokio::test]
async fn resume_and_fill_start_and_bind_runs() {
    let d = daemon();
    let r = &d.router;
    let seat = create(r, "lead", "claude-code").await;
    let id = seat["id"].as_str().unwrap().to_string();

    // An explicit resume of a never-filled seat never falls back (§6.2).
    let e = refused(
        r,
        "seats_resume",
        json!({ "seatId": id, "prompt": "hi", "actor": ui(), "opts": { "fallback": "refuse" } }),
        "not_resumable",
    )
    .await;
    assert_eq!(e["error_data"]["details"]["reason"], "no_session");

    let resumed = ok(
        r,
        "seats_resume",
        json!({ "seatId": id, "prompt": "hello", "actor": ui(), "opts": { "fallback": "fresh" } }),
    )
    .await;
    assert_eq!(resumed["outcome"], "started-fresh");
    assert_eq!(resumed["reason"], "no_session");
    let run1 = resumed["run_id"].as_str().unwrap().to_string();
    assert_eq!(resumed["seat"]["session"]["run_id"], run1.as_str());
    wait_done(r, &run1).await;
    let row = crate::server::shared::chi::cache_get(&d.db, &run1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.owner, "seat");

    let view = ok(r, "seats_get", json!({ "seat": { "seatId": id } })).await;
    assert_eq!(view["status"], "idle", "{view}");
    assert_eq!(view["session"]["external_id"], "sess-stub");
    refused(
        r,
        "seats_resume",
        json!({ "seatId": id, "prompt": "again", "actor": ui(), "opts": { "fallback": "fresh" } }),
        "seat_not_vacant",
    )
    .await;
    let route = ok(
        r,
        "seats_resolve",
        json!({ "seat": { "seatId": id }, "actor": ui() }),
    )
    .await;
    assert_eq!(route["route"], "chi-resume");
    assert_eq!(route["run_id"], run1.as_str());
    assert_eq!(route["busy"], false);

    // Fill: a new run, the old one unseated and reported.
    let filled = ok(
        r,
        "seats_fill",
        json!({ "seatId": id, "prompt": "fresh start", "actor": ui(), "opts": null }),
    )
    .await;
    let run2 = filled["run_id"].as_str().unwrap().to_string();
    assert_ne!(run2, run1);
    assert_eq!(filled["previous"]["run_id"], run1.as_str());
    assert_eq!(filled["seat"]["session"]["run_id"], run2.as_str());
    wait_done(r, &run2).await;
}

/// A run started with `chi_run` moves into a seat, and on into another,
/// unseating it from the first atomically (DEC-69c).
#[cfg(unix)]
#[tokio::test]
async fn a_chi_run_moves_between_seats() {
    let d = daemon();
    let r = &d.router;
    let first = create(r, "first", "claude-code").await;
    let second = create(r, "second", "claude-code").await;
    let started = ok(
        r,
        "chi_run",
        json!({ "opts": { "engineId": "claude-code", "prompt": "side" } }),
    )
    .await;
    let run = started["run_id"].as_str().unwrap().to_string();
    wait_done(r, &run).await;
    let session = json!({ "kind": "run", "runId": run });
    let moved = ok(
        r,
        "seats_move",
        json!({ "session": session, "toSeatId": first["id"], "actor": ui() }),
    )
    .await;
    assert_eq!(moved["seat"]["session"]["run_id"], run.as_str());
    assert_eq!(moved["seat"]["status"], "idle");
    assert_eq!(moved["from_seat_ids"], json!([]));
    let moved = ok(
        r,
        "seats_move",
        json!({ "session": session, "to_seat_id": second["id"], "actor": ui(), "opts": null }),
    )
    .await;
    assert_eq!(moved["from_seat_ids"], json!([first["id"]]));
    let first = ok(r, "seats_get", json!({ "seat": { "seatId": first["id"] } })).await;
    assert_eq!(first["session"], Value::Null);
    // A run on another engine can't sit in a claude seat.
    let _ = rpc(
        r,
        "chi_run",
        json!({ "opts": { "engineId": "cursor-agent", "prompt": "hi", "cwd": "/tmp" } }),
    )
    .await;
    let other = ok(r, "chi_list", json!({ "engineId": "cursor-agent" })).await[0]["run_id"].clone();
    refused(
        r,
        "seats_move",
        json!({ "session": { "kind": "run", "runId": other }, "toSeatId": first["id"], "actor": ui() }),
        "engine_mismatch",
    )
    .await;
}

/// §4.5 on the daemon: a queued text is held (`queued`, one slot) until the
/// seat's run is out of flight, then this process's poller sends it through
/// `chi_resume`'s core, on the same run.
#[cfg(unix)]
#[tokio::test]
async fn the_poller_sends_a_queued_text_on_the_same_run() {
    let d = daemon();
    let r = &d.router;
    let seat = create(r, "lead", "claude-code").await;
    let id = seat["id"].as_str().unwrap().to_string();

    // A seat with no run has nothing to queue for.
    refused(
        r,
        "seats_queue",
        json!({ "seatId": id, "prompt": "x", "actor": ui() }),
        "conflict",
    )
    .await;

    let filled = ok(
        r,
        "seats_fill",
        json!({ "seatId": id, "prompt": "hello", "actor": ui() }),
    )
    .await;
    let run_id = filled["run_id"].as_str().unwrap().to_string();
    wait_done(r, &run_id).await;
    let queued = ok(
        r,
        "seats_queue",
        json!({ "seatId": id, "prompt": "after that", "actor": ui() }),
    )
    .await;
    assert!(queued["queued"]["since"].is_i64(), "{queued}");
    assert!(
        chi_exec::tests::wait_for(|| async {
            let s = run_status(r, &run_id).await;
            s["status"] == "done"
                && s["output"]
                    .as_str()
                    .is_some_and(|o| o.contains("--resume sess-stub"))
        })
        .await,
        "the queued text was never sent"
    );
    let view = ok(r, "seats_get", json!({ "seat": { "seatId": id } })).await;
    assert_eq!(view["queued"], Value::Null);
    assert_eq!(view["session"]["run_id"], run_id.as_str());
}

/// A text queued behind a run that then can't take it (cancelled before its
/// engine reported a session id) is dropped, not kept forever: the slot
/// empties and the next text can be queued.
#[cfg(unix)]
#[tokio::test]
async fn a_queued_text_the_run_cannot_take_is_dropped() {
    let d = daemon();
    let r = &d.router;
    let seat = create(r, "lead", "claude-code").await;
    let id = seat["id"].as_str().unwrap().to_string();
    // `sleep` keeps the stub engine running until cancelled.
    let filled = ok(
        r,
        "seats_fill",
        json!({ "seatId": id, "prompt": "please sleep", "actor": ui() }),
    )
    .await;
    let run_id = filled["run_id"].as_str().unwrap().to_string();
    assert_eq!(filled["seat"]["status"], "run");
    ok(
        r,
        "seats_queue",
        json!({ "seatId": id, "prompt": "after that", "actor": ui() }),
    )
    .await;
    refused(
        r,
        "seats_queue",
        json!({ "seatId": id, "prompt": "and more", "actor": ui() }),
        "seat_busy",
    )
    .await;
    // Still in flight: the poller holds the text.
    tokio::time::sleep(QUEUE_POLL * 2).await;
    let view = ok(r, "seats_get", json!({ "seat": { "seatId": id } })).await;
    assert!(view["queued"].is_object(), "{view}");

    ok(r, "chi_cancel", json!({ "runId": run_id })).await;
    assert!(
        chi_exec::tests::wait_for(|| async {
            ok(r, "seats_get", json!({ "seat": { "seatId": id } })).await["queued"].is_null()
        })
        .await,
        "the undeliverable text was never dropped"
    );
    assert_eq!(run_status(r, &run_id).await["status"], "cancelled");
}

// ── per-principal isolation ─────────────────────────────────────────────

/// Under T1 every principal is its own daemon process with its own
/// `--data-dir` and `ikenga.db`. One principal's seats never appear in
/// another's roster, and no read or write reaches them by id or address:
/// the answer is the same `seat_not_found` an unknown seat gets (no
/// existence oracle). Same names are independent per principal.
#[tokio::test]
async fn another_principals_seats_are_unreachable() {
    let ada = daemon();
    let bob = daemon();
    let seat = create(&ada.router, "lead", "claude-code").await;
    let id = seat["id"].as_str().unwrap().to_string();

    let r = &bob.router;
    assert_eq!(list(r).await, Vec::<Value>::new());
    for seat in [
        json!({ "seatId": id }),
        json!({ "address": "seat:default/lead" }),
        json!({ "address": "lead" }),
    ] {
        refused(r, "seats_get", json!({ "seat": seat }), "seat_not_found").await;
        refused(
            r,
            "seats_resolve",
            json!({ "seat": seat, "actor": { "client": "bob", "hold": true, "takeover": true } }),
            "seat_not_found",
        )
        .await;
    }
    let actor = json!({ "client": "bob", "takeover": true });
    for (cmd, args) in [
        (
            "seats_rename",
            json!({ "seatId": id, "name": "mine", "actor": actor }),
        ),
        ("seats_clear", json!({ "seatId": id, "actor": actor })),
        ("seats_release", json!({ "seatId": id, "actor": actor })),
        (
            "seats_remove",
            json!({ "seatId": id, "opts": { "removeMemory": true }, "actor": actor }),
        ),
        (
            "seats_queue",
            json!({ "seatId": id, "prompt": "x", "actor": actor }),
        ),
        (
            "seats_resume",
            json!({ "seatId": id, "prompt": "x", "actor": actor, "opts": { "fallback": "fresh" } }),
        ),
        (
            "seats_fill",
            json!({ "seatId": id, "prompt": "x", "actor": actor }),
        ),
    ] {
        refused(r, cmd, args, "seat_not_found").await;
    }
    // A run of bob's can't be moved into ada's seat either.
    let _ = rpc(
        r,
        "chi_run",
        json!({ "opts": { "engineId": "cursor-agent", "prompt": "hi", "cwd": "/tmp" } }),
    )
    .await;
    let bob_run = ok(r, "chi_list", json!({ "engineId": "cursor-agent" })).await[0]["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    refused(
        r,
        "seats_move",
        json!({ "session": { "kind": "run", "runId": bob_run }, "toSeatId": id, "actor": actor }),
        "seat_not_found",
    )
    .await;
    // Nor ada's run into a seat of bob's: it is not one of his runs.
    let bob_seat = create(r, "lead", "claude-code").await;
    assert_ne!(bob_seat["id"], id.as_str(), "names are per principal");
    let _ = rpc(
        &ada.router,
        "chi_run",
        json!({ "opts": { "engineId": "cursor-agent", "prompt": "hi", "cwd": "/tmp" } }),
    )
    .await;
    let ada_run = ok(
        &ada.router,
        "chi_list",
        json!({ "engineId": "cursor-agent" }),
    )
    .await[0]["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let e = refused(
        r,
        "seats_move",
        json!({ "session": { "kind": "run", "runId": ada_run }, "toSeatId": bob_seat["id"], "actor": ui() }),
        "conflict",
    )
    .await;
    assert_eq!(
        e["error_data"]["message"],
        format!("chi run not found: {ada_run}")
    );

    // Ada's seat is untouched: same name, no hold, no session.
    let mine = list(&ada.router).await;
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0]["id"], id.as_str());
    assert_eq!(mine[0]["name"], "lead");
    assert_eq!(mine[0]["hold"], Value::Null);
    assert_eq!(mine[0]["session"], Value::Null);
    let bobs = list(r).await;
    assert_eq!(bobs.len(), 1);
    assert_eq!(bobs[0]["id"], bob_seat["id"]);
}
