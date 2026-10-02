//! Reseal (G-ACCESS §6.4), WP-77: `access_audit_reseal {ackSeq}` (T0, the
//! operator bearer only — in practice the desktop) and
//! `ikenga-server audit reseal --ack <seq>` (T1, root).
//!
//! A reseal acknowledges the **outstanding** break (the first one no
//! earlier reseal acknowledged): `ackSeq` must name it, so an operator
//! can't wave through a break they haven't looked at. It appends
//! `audit.resealed {broken_at_seq, prior_head}` after the DB head and
//! re-walks the chain: `degraded` clears unless another unacknowledged
//! break remains (then that one is outstanding). The break itself stays in
//! the chain for ever; every later walk sees it, acknowledged.
//!
//! A T1 broker that is degraded notices a root-CLI reseal on its next
//! append (`Chain::append` step 0) and resumes from the reseal row.

use serde_json::{json, Value};
use sqlx::Connection;

use super::chain::{db_head, insert_row, Broken, Head};
use super::list::store;
use super::{verify_boot, Event};
use crate::access::ctx::AccessCtx;
use crate::access::rpc::Env;
use crate::access::store::{AccessStore, StoreTier};
use crate::access::{AccessError, Code};

/// What a reseal did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resealed {
    pub broken_at_seq: i64,
    /// The `audit.resealed` row.
    pub head: Head,
    /// Another unacknowledged break, if one remains.
    pub still_broken: Option<Broken>,
}

/// The reseal core: `ev` is the `audit.resealed` row with its actor
/// columns set; this adds `{broken_at_seq, prior_head}`.
pub async fn reseal(store: &AccessStore, ack_seq: i64, ev: Event) -> Result<Resealed, AccessError> {
    debug_assert_eq!(ev.kind, "audit.resealed");
    let chain = store.chain();
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    // Bring the process's state up to date first (a break it found but
    // couldn't record yet is written, so the walk below sees it).
    chain
        .verify_boot(&mut conn)
        .await
        .map_err(AccessError::internal)?;
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(AccessError::internal)?;
    let verdict = verify_boot::verify(&mut tx, chain.store_id())
        .await
        .map_err(AccessError::internal)?;
    let Some(b) = verdict.outstanding else {
        return Err(AccessError::new(
            Code::Conflict,
            "the audit chain is not broken; there is nothing to reseal",
        ));
    };
    if b.broken_at_seq != ack_seq {
        return Err(AccessError::new(
            Code::Conflict,
            format!(
                "the audit chain is broken at #{}, not #{ack_seq}; acknowledge that one",
                b.broken_at_seq
            ),
        ));
    }
    let prior = db_head(&mut tx).await.map_err(AccessError::internal)?;
    let ev = ev.detail(json!({
        "broken_at_seq": ack_seq,
        "reason": b.reason,
        "prior_head": prior.map(|h| json!({ "seq": h.seq, "hash": hex::encode(h.hash) })),
    }));
    let head = insert_row(&mut tx, chain.store_id(), prior, &ev)
        .await
        .map_err(AccessError::internal)?;
    tx.commit().await.map_err(AccessError::internal)?;
    chain.committed(head);
    tracing::warn!(
        "audit chain: the break at #{ack_seq} was resealed at #{} (it stays in the chain)",
        head.seq
    );
    let report = chain
        .verify_boot(&mut conn)
        .await
        .map_err(AccessError::internal)?;
    Ok(Resealed {
        broken_at_seq: ack_seq,
        head,
        still_broken: report.broken,
    })
}

/// `access_audit_reseal {ackSeq}` → `{}` (§9.1): the T0 operator bearer
/// only (T1: the root CLI).
pub async fn dispatch(env: &Env<'_>, ctx: &AccessCtx, args: &Value) -> Result<Value, AccessError> {
    if env.tier != StoreTier::T0 || !ctx.is_operator() {
        return Err(AccessError::new(Code::Forbidden, "class=operator"));
    }
    let ack = args
        .get("ackSeq")
        .and_then(Value::as_i64)
        .filter(|s| *s > 0)
        .ok_or_else(|| AccessError::new(Code::InvalidRequest, "`ackSeq` must be a positive seq"))?;
    let store = store(env)?;
    reseal(store, ack, Event::by("audit.resealed", ctx)).await?;
    Ok(json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::audit::list::tests::env;
    use crate::access::devices::tests::{operator_ctx, pair};
    use crate::access::rpc::dispatch as rpc;
    use crate::access::sockets::Registry;
    use crate::access::Tier;

    /// Break the chain the way an attacker with DB access would.
    pub(crate) async fn forge(store: &AccessStore, seq: i64) {
        sqlx::raw_sql("DROP TRIGGER audit_events_no_update; DROP TRIGGER audit_events_no_delete;")
            .execute(store.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE audit_events SET target = 'forged' WHERE seq = ?")
            .bind(seq)
            .execute(store.pool())
            .await
            .unwrap();
    }

    /// A-17: with a broken chain every access-changing arm answers
    /// `audit_unavailable`, while decisions and desktop-local rows keep
    /// appending after the DB head; the reseal clears it.
    #[tokio::test]
    async fn degraded_mode_pauses_access_changes_until_resealed() {
        let store = AccessStore::memory_t0().await;
        let reg = Registry::new();
        let e = env(&store, &reg);
        let op = operator_ctx(&store);
        let (row, _) = pair(&store, Tier::Dispatch).await;
        // Row 1 (store.created): `pair` writes its device row directly.
        forge(&store, 1).await;
        let v = rpc(&e, &op, "access_audit_verify", &json!({}))
            .await
            .unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["brokenAtSeq"], 1);
        assert_eq!(store.status(), ("degraded", Some(1)));

        let refused = rpc(
            &e,
            &op,
            "access_device_set_tier",
            &json!({"deviceId": row.device_id, "tier": "approve"}),
        )
        .await
        .unwrap_err();
        assert_eq!(refused.code, Code::AuditUnavailable);
        let refused = rpc(
            &e,
            &op,
            "access_device_revoke",
            &json!({"deviceId": row.device_id}),
        )
        .await
        .unwrap_err();
        assert_eq!(refused.code, Code::AuditUnavailable);
        // Decisions and desktop-local rows continue (P-35).
        rpc(
            &e,
            &op,
            "access_audit_record_local",
            &json!({"kind": "permission.decided", "target": "Read STATUS.md"}),
        )
        .await
        .unwrap();
        rpc(
            &e,
            &op,
            "access_audit_record_local",
            &json!({"kind": "vault.locked", "target": "workspace"}),
        )
        .await
        .unwrap();
        // The tier is unchanged.
        let tier: String = sqlx::query_scalar("SELECT tier FROM devices WHERE device_id = ?")
            .bind(&row.device_id)
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(tier, "dispatch");

        // A wrong ack is refused; a device can't reseal.
        let wrong = rpc(&e, &op, "access_audit_reseal", &json!({"ackSeq": 2}))
            .await
            .unwrap_err();
        assert_eq!(wrong.code, Code::Conflict);
        let phone =
            crate::access::audit::list::tests::device_ctx(&store, &row.device_id, Tier::Full);
        let no = rpc(&e, &phone, "access_audit_reseal", &json!({"ackSeq": 1}))
            .await
            .unwrap_err();
        assert_eq!(no.code, Code::Forbidden);

        rpc(&e, &op, "access_audit_reseal", &json!({"ackSeq": 1}))
            .await
            .unwrap();
        assert_eq!(store.status(), ("ok", None));
        rpc(
            &e,
            &op,
            "access_device_set_tier",
            &json!({"deviceId": row.device_id, "tier": "approve"}),
        )
        .await
        .unwrap();
        // The break stays visible; the walk now accepts it.
        let v = rpc(&e, &op, "access_audit_verify", &json!({}))
            .await
            .unwrap();
        assert_eq!(v["ok"], true);
        let kinds: Vec<String> = sqlx::query_scalar(
            "SELECT kind FROM audit_events WHERE kind LIKE 'audit.%' ORDER BY seq",
        )
        .fetch_all(store.pool())
        .await
        .unwrap();
        assert_eq!(
            kinds,
            [
                "audit.chain_broken",
                "audit.verified",
                "audit.resealed",
                "audit.verified"
            ]
        );
        let again = rpc(&e, &op, "access_audit_reseal", &json!({"ackSeq": 1}))
            .await
            .unwrap_err();
        assert_eq!(again.code, Code::Conflict);
    }

    /// A-37: a root-CLI append between two broker appends doesn't degrade
    /// the broker; a root-CLI reseal un-degrades it on its next append.
    #[tokio::test]
    async fn another_writer_appends_and_reseals_under_a_running_process() {
        let store = AccessStore::memory_t0().await;
        let (row, _) = pair(&store, Tier::Dispatch).await;
        // The "CLI": a second Chain over the same file.
        let cli = crate::access::audit::chain::Chain::new(store.meta().store_id.clone());
        {
            let mut conn = store.pool().acquire().await.unwrap();
            let mut tx = conn.begin().await.unwrap();
            cli.append(
                &mut tx,
                &Event::new("auth.account_created", crate::access::audit::AuditVia::Cli),
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
        }
        let op = operator_ctx(&store);
        crate::access::devices::set_tier(&store, &op, &row.device_id, Tier::Approve)
            .await
            .unwrap();
        assert!(store.chain().degraded().is_none());

        // Break it; the running process notices at its next verify.
        forge(&store, 3).await;
        {
            let mut conn = store.pool().acquire().await.unwrap();
            store.chain().verify_boot(&mut conn).await.unwrap();
        }
        assert_eq!(store.chain().degraded().unwrap().broken_at_seq, 3);
        // The "CLI" reseals through its own store handle on the same pool.
        let cli_store = store.clone_with_fresh_chain();
        reseal(
            &cli_store,
            3,
            Event::new("audit.resealed", crate::access::audit::AuditVia::Cli),
        )
        .await
        .unwrap();
        // The running process resumes on its next access change.
        crate::access::devices::set_tier(&store, &op, &row.device_id, Tier::Dispatch)
            .await
            .unwrap();
        assert!(store.chain().degraded().is_none());
    }
}
