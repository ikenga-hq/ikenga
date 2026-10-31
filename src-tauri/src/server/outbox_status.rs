//! `outbox_approvals_status` — is the box's sender approvals backend live?
//!
//! The shell shows its Outbox entry in the Explorer only where approvals can
//! actually be served. On the box that means the sender add-on's socket
//! (`ApprovalsConfig::from_env()`, default `/run/ikenga-sender/approvals.sock`)
//! exists, accepts a connection, and is served by a uid below the agent
//! floor: the same rule the broker applies before it trusts a reply. The
//! probe connects and reads the peer credentials; it writes nothing.
//!
//! Anywhere that is not Linux there is no sender socket, so the answer is
//! always `false` (the desktop uses its own local signal).

use serde_json::{json, Value};

/// The arm's answer: `{ "active": bool }`.
pub async fn status() -> Value {
    json!({ "active": probe().await })
}

#[cfg(target_os = "linux")]
async fn probe() -> bool {
    let cfg = crate::server::broker::approvals::ApprovalsConfig::from_env();
    socket_is_sender(&cfg.socket, cfg.agent_uid_min).await
}

#[cfg(not(target_os = "linux"))]
async fn probe() -> bool {
    false
}

/// True when `socket` accepts a connection within the broker's connect
/// timeout and its server uid is below `agent_uid_min`.
#[cfg(target_os = "linux")]
async fn socket_is_sender(socket: &std::path::Path, agent_uid_min: u32) -> bool {
    use std::time::Duration;
    let connect = tokio::net::UnixStream::connect(socket);
    match tokio::time::timeout(Duration::from_secs(3), connect).await {
        Ok(Ok(stream)) => matches!(stream.peer_cred(), Ok(c) if c.uid() < agent_uid_min),
        _ => false,
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::server::broker::approvals::DEFAULT_AGENT_UID_MIN;

    #[tokio::test]
    async fn a_missing_socket_is_inactive() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!socket_is_sender(&dir.path().join("none.sock"), DEFAULT_AGENT_UID_MIN).await);
    }

    #[tokio::test]
    async fn a_plain_file_is_inactive() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("plain");
        std::fs::write(&f, b"x").unwrap();
        assert!(!socket_is_sender(&f, DEFAULT_AGENT_UID_MIN).await);
    }

    #[tokio::test]
    async fn a_socket_served_by_an_allowed_uid_is_active() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.sock");
        let _l = tokio::net::UnixListener::bind(&p).unwrap();
        assert!(socket_is_sender(&p, u32::MAX).await);
    }

    #[tokio::test]
    async fn a_socket_served_by_an_agent_uid_is_inactive() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.sock");
        let _l = tokio::net::UnixListener::bind(&p).unwrap();
        // uid floor 0: the test process itself counts as an agent account.
        assert!(!socket_is_sender(&p, 0).await);
    }
}
