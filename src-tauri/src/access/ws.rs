//! WebSocket frame checks (G-ACCESS §1.6 "Non-RPC routes", §1.7).
//!
//! The attach itself needs `sessions` (PTY, chat) or `files` (fs) — checked
//! at the handshake (`access::route_requirement`). Then, per client frame:
//!
//! * `/ws/pty/:id` — every input frame (binary stdin, `Write`, `Resize`,
//!   `Kill`, raw text) and `?spawn=true` need `dispatch`;
//! * `/ws/chat/:id` — `Prompt` and `Cancel` need `dispatch`;
//! * `/ws/fs` — no frame needs more than the attach.
//!
//! A forbidden frame is **dropped** and answered with the control frame
//! `{"type":"error","code":"forbidden","missing":["dispatch"]}`; the socket
//! stays open. The same check runs on the T0 daemon, in the T1 broker's
//! frame hook (before proxying) and in the T1 child (caps from
//! `X-Ikenga-Caps`, defence in depth).

use super::caps::{Cap, CapSet};

/// Which socket a frame belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Pty,
    Chat,
    Fs,
    Other,
}

impl Route {
    pub fn of(path: &str) -> Route {
        if path.starts_with("/ws/pty/") {
            Route::Pty
        } else if path.starts_with("/ws/chat/") {
            Route::Chat
        } else if path == "/ws/fs" || path.starts_with("/ws/fs?") {
            Route::Fs
        } else {
            Route::Other
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Route::Pty => "pty",
            Route::Chat => "chat",
            Route::Fs => "fs",
            Route::Other => "other",
        }
    }
}

/// One client → server data frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame<'a> {
    Text(&'a str),
    Binary(&'a [u8]),
}

/// The caps one frame needs.
pub fn frame_needs(route: Route, frame: Frame<'_>) -> CapSet {
    let dispatch = CapSet::of(&[Cap::Dispatch]);
    match route {
        // Binary is raw stdin; every PTY text frame is a write, a resize or a
        // kill (or the raw-text write fallback) — all of them act.
        Route::Pty => dispatch,
        Route::Chat => match frame {
            Frame::Text(t) => {
                let kind = serde_json::from_str::<serde_json::Value>(t)
                    .ok()
                    .and_then(|v| v.get("type").and_then(|k| k.as_str()).map(str::to_string));
                match kind.as_deref() {
                    Some("prompt") | Some("cancel") => dispatch,
                    _ => CapSet::EMPTY,
                }
            }
            Frame::Binary(_) => CapSet::EMPTY,
        },
        Route::Fs | Route::Other => CapSet::EMPTY,
    }
}

/// `Ok` to deliver, or the caps the frame lacks.
pub fn check_frame(caps: CapSet, route: Route, frame: Frame<'_>) -> Result<(), CapSet> {
    let missing = caps.missing(frame_needs(route, frame));
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

/// The refusal control frame for a dropped frame.
pub fn refusal(missing: CapSet) -> String {
    serde_json::json!({ "type": "error", "code": "forbidden", "missing": missing.names() })
        .to_string()
}

/// Whether this frame starts or steers work (the WP-77 dispatch-audit hook
/// counts these, P-22).
pub fn is_dispatch(route: Route, frame: Frame<'_>) -> bool {
    frame_needs(route, frame).contains(Cap::Dispatch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::caps::Tier;

    #[test]
    fn pty_input_needs_dispatch_chat_prompts_too_fs_nothing() {
        let view = Tier::View.caps();
        let dispatch = Tier::Dispatch.caps();
        for f in [
            Frame::Binary(b"ls\n"),
            Frame::Text(r#"{"type":"write","data":"x"}"#),
            Frame::Text(r#"{"type":"resize","rows":1,"cols":1}"#),
            Frame::Text(r#"{"type":"kill"}"#),
            Frame::Text("raw text"),
        ] {
            assert_eq!(
                check_frame(view, Route::Pty, f),
                Err(CapSet::of(&[Cap::Dispatch]))
            );
            assert_eq!(check_frame(dispatch, Route::Pty, f), Ok(()));
        }
        for t in [r#"{"type":"prompt","prompt":"hi"}"#, r#"{"type":"cancel"}"#] {
            assert!(check_frame(view, Route::Chat, Frame::Text(t)).is_err());
            assert!(is_dispatch(Route::Chat, Frame::Text(t)));
        }
        assert!(check_frame(view, Route::Chat, Frame::Text("{}")).is_ok());
        assert!(check_frame(
            CapSet::EMPTY,
            Route::Fs,
            Frame::Text(r#"{"type":"watch","path":"/"}"#)
        )
        .is_ok());
    }

    #[test]
    fn the_refusal_frame_names_what_is_missing() {
        let v: serde_json::Value =
            serde_json::from_str(&refusal(CapSet::of(&[Cap::Dispatch]))).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"type":"error","code":"forbidden","missing":["dispatch"]})
        );
        assert_eq!(Route::of("/ws/pty/abc"), Route::Pty);
        assert_eq!(Route::of("/ws/fs"), Route::Fs);
        assert_eq!(Route::of("/ws/chat/t1"), Route::Chat);
    }
}
