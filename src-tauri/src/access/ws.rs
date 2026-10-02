//! WebSocket checks (G-ACCESS §1.6 "Non-RPC routes", §1.7).
//!
//! * **Handshake:** `/ws/pty/:id` owner{sessions} (`?spawn=true` also needs
//!   {dispatch}); `/ws/chat/:id` shared{sessions}; `/ws/fs` shared{files}.
//! * **Frames:** PTY `Write` / `Resize` / `Kill`, raw text and binary stdin
//!   need {dispatch}; chat `Prompt` / `Cancel` need {dispatch}. A refused
//!   frame is dropped and answered with
//!   `{"type":"error","code":"forbidden","missing":["dispatch"]}`; the socket
//!   stays open.
//!
//! The same [`check_frame`] runs in the T0 daemon / T1 child handlers and
//! in the T1 broker's proxy hook (defence in depth: a caps header only
//! narrows).

use serde_json::json;

use super::caps::{ArmClass, Cap, CapSet, Requirement};
use super::ctx::AccessCtx;
use super::rpc_requirements::route_requirement;

/// Which socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsRoute {
    Pty,
    Chat,
    Fs,
}

impl WsRoute {
    pub fn of_path(path: &str) -> Option<WsRoute> {
        if path.starts_with("/ws/pty/") {
            Some(WsRoute::Pty)
        } else if path.starts_with("/ws/chat/") {
            Some(WsRoute::Chat)
        } else if path == "/ws/fs" {
            Some(WsRoute::Fs)
        } else {
            None
        }
    }
}

/// One client → server data frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame<'a> {
    Text(&'a str),
    Binary(&'a [u8]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameVerdict {
    Pass,
    /// Drop the frame and answer with [`refusal_frame`].
    Refuse(CapSet),
}

const DISPATCH: CapSet = CapSet::of(&[Cap::Dispatch]);

fn need(caps: CapSet, required: CapSet) -> FrameVerdict {
    let missing = caps.missing(required);
    if missing.is_empty() {
        FrameVerdict::Pass
    } else {
        FrameVerdict::Refuse(missing)
    }
}

/// `access::ws::check_frame` (§9.2): what a frame needs on its route.
pub fn check_frame(caps: CapSet, route: WsRoute, frame: Frame<'_>) -> FrameVerdict {
    match route {
        // Every PTY data frame acts on the terminal: binary is stdin, text is
        // a control frame (`resize` / `write` / `kill`) or raw stdin.
        WsRoute::Pty => need(caps, DISPATCH),
        WsRoute::Chat => match frame {
            // The handler's own parser decides what a frame is, so an
            // escaped or duplicated `type` can't make the check and the
            // handler disagree: anything it would act on (`Prompt`,
            // `Cancel`) needs {dispatch}; anything it ignores passes.
            Frame::Text(t) => {
                match serde_json::from_str::<crate::server::chat_ws::ChatClientMessage>(t) {
                    Ok(_) => need(caps, DISPATCH),
                    Err(_) => FrameVerdict::Pass,
                }
            }
            Frame::Binary(_) => FrameVerdict::Pass,
        },
        // Watch roots are confined by `share::fs_watch_root`; the handshake
        // already required {files}.
        WsRoute::Fs => FrameVerdict::Pass,
    }
}

/// The control frame a refused frame is answered with.
pub fn refusal_frame(missing: CapSet) -> String {
    json!({ "type": "error", "code": "forbidden", "missing": missing.names() }).to_string()
}

/// Whether a PTY attach may spawn (`PtyQuery::spawn`). The query is
/// percent- and `+`-decoded exactly as the handler's `Query<PtyQuery>`
/// (`serde_urlencoded` over `form_urlencoded`) decodes it, so
/// `sp%61wn=tru%65` is a spawn here too. Fail closed: any decoded `spawn`
/// key whose value is not exactly `false` counts — duplicates included,
/// and a value the handler would reject still needs {dispatch}.
pub fn wants_spawn(query: Option<&str>) -> bool {
    query.is_some_and(|q| {
        url::form_urlencoded::parse(q.as_bytes()).any(|(k, v)| k == "spawn" && v != "false")
    })
}

/// Check `ctx` against a requirement (§1.6 rule 4 for non-RPC routes).
pub fn check_requirement(ctx: &AccessCtx, req: Requirement) -> Result<(), String> {
    if ctx.via == crate::access::ctx::Credential::Unresolved {
        return Err("unauthenticated: no usable credential".into());
    }
    match req.class {
        ArmClass::Operator if !ctx.is_operator() => return Err("forbidden: class=operator".into()),
        ArmClass::Owner if ctx.share.is_some() => return Err("forbidden: class=owner".into()),
        ArmClass::Internal | ArmClass::Access => return Err("forbidden: class=internal".into()),
        _ => {}
    }
    let missing = ctx.caps.missing(req.caps);
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("forbidden: missing={}", missing.to_header()))
    }
}

/// The handshake / HTTP route check for every protected non-RPC route.
/// Routes this table doesn't govern pass.
pub fn authorize_route(ctx: &AccessCtx, path: &str, query: Option<&str>) -> Result<(), String> {
    let Some(mut req) = route_requirement(path) else {
        return Ok(());
    };
    if WsRoute::of_path(path) == Some(WsRoute::Pty) && wants_spawn(query) {
        // (re-checked on the decoded `PtyQuery` in `pty_ws_handler`)
        req.caps = req.caps.with(Cap::Dispatch);
    }
    check_requirement(ctx, req)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::caps::Tier;
    use crate::executor::PrincipalId;

    fn dev(tier: Tier) -> AccessCtx {
        AccessCtx::device(PrincipalId::new_v7(), "d".into(), tier, 0)
    }

    #[test]
    fn pty_input_needs_dispatch_in_every_form() {
        let view = Tier::View.caps();
        for f in [
            Frame::Binary(b"ls\n"),
            Frame::Text(r#"{"type":"write","data":"ls"}"#),
            Frame::Text(r#"{"type":"resize","rows":1,"cols":1}"#),
            Frame::Text(r#"{"type":"kill"}"#),
            Frame::Text("raw keystrokes"),
        ] {
            assert_eq!(
                check_frame(view, WsRoute::Pty, f),
                FrameVerdict::Refuse(DISPATCH),
                "{f:?}"
            );
            assert_eq!(
                check_frame(Tier::Dispatch.caps(), WsRoute::Pty, f),
                FrameVerdict::Pass
            );
        }
    }

    /// Review finding 1: the check decodes the query as the handler does.
    #[test]
    fn spawn_is_read_from_the_decoded_query() {
        for q in [
            "spawn=true",
            "sp%61wn=true",
            "spawn=tru%65",
            "%73%70%61%77%6e=%74rue",
            "spawn=1",
            "spawn",
            "spawn=false&spawn=true",
            "spawn=true&spawn=false",
            "spawn=%FF",
            "x=1&spawn=+true",
        ] {
            assert!(wants_spawn(Some(q)), "{q}");
            assert!(
                authorize_route(&dev(Tier::View), "/ws/pty/new", Some(q))
                    .unwrap_err()
                    .contains("missing=dispatch"),
                "{q}"
            );
        }
        for q in [
            "",
            "spawn=false",
            "spawned=true",
            "x=spawn",
            "sp%61wn=false",
        ] {
            assert!(!wants_spawn(Some(q)), "{q}");
        }
        assert!(!wants_spawn(None));
        // What the handler decodes agrees on the encoded forms.
        #[derive(serde::Deserialize)]
        struct Q {
            #[serde(default)]
            spawn: bool,
        }
        for q in ["sp%61wn=true", "spawn=tru%65"] {
            let uri: axum::http::Uri = format!("/ws/pty/x?{q}").parse().unwrap();
            let parsed = axum::extract::Query::<Q>::try_from_uri(&uri).unwrap();
            assert!(parsed.spawn && wants_spawn(Some(q)), "{q}");
        }
    }

    #[test]
    fn unknown_socket_paths_fail_closed() {
        assert!(authorize_route(&dev(Tier::Full), "/ws/%70ty/x", None).is_ok());
        assert!(authorize_route(&dev(Tier::Approve), "/ws/%70ty/x", None).is_err());
    }

    #[test]
    fn chat_frames_are_classified_by_the_handlers_parser() {
        let view = Tier::View.caps();
        for t in [r#"{"type":"pro\u006dpt","prompt":"hi"}"#] {
            assert_ne!(
                check_frame(view, WsRoute::Chat, Frame::Text(t)),
                FrameVerdict::Pass,
                "{t}"
            );
        }
    }

    #[test]
    fn chat_prompt_and_cancel_need_dispatch() {
        let view = Tier::View.caps();
        for t in [r#"{"type":"prompt","prompt":"hi"}"#, r#"{"type":"cancel"}"#] {
            assert_eq!(
                check_frame(view, WsRoute::Chat, Frame::Text(t)),
                FrameVerdict::Refuse(DISPATCH)
            );
        }
        assert_eq!(
            check_frame(view, WsRoute::Chat, Frame::Text("{}")),
            FrameVerdict::Pass
        );
        let v: serde_json::Value = serde_json::from_str(&refusal_frame(DISPATCH)).unwrap();
        assert_eq!(
            v,
            json!({"type":"error","code":"forbidden","missing":["dispatch"]})
        );
    }

    #[test]
    fn handshakes_follow_the_route_table() {
        let view = dev(Tier::View);
        assert!(authorize_route(&view, "/ws/pty/t1", None).is_ok());
        assert!(authorize_route(&view, "/ws/pty/t1", Some("spawn=true"))
            .unwrap_err()
            .contains("missing=dispatch"));
        assert!(
            authorize_route(&dev(Tier::Dispatch), "/ws/pty/t1", Some("cols=80&spawn=1")).is_ok()
        );
        assert!(authorize_route(&view, "/ws/chat/x", None).is_ok());
        assert!(authorize_route(&view, "/ws/fs", None).is_ok());
        assert!(authorize_route(&view, "/pkgs/a/b.js", None).is_ok());
        // Only the operator bearer may shut the daemon down.
        assert!(authorize_route(&dev(Tier::Full), "/api/shutdown", None)
            .unwrap_err()
            .contains("class=operator"));
        let op = AccessCtx::operator(None, None);
        assert!(authorize_route(&op, "/api/shutdown", None).is_ok());
        // A child request without caps reaches nothing.
        let child = AccessCtx::child(None, None);
        assert!(authorize_route(&child, "/ws/fs", None).is_err());
        assert!(authorize_route(&child, "/pkgs/a/x", None).is_err());
        // The broker's non-granting context reaches no route at all.
        assert!(authorize_route(&AccessCtx::nothing(), "/ws/chat/x", None).is_err());
    }
}
