//! Reseal (G-ACCESS §6.4), WP-77: `access_audit_reseal {ackSeq}` (T0, the
//! operator bearer only — in practice the desktop) and
//! `ikenga-server audit reseal --ack <seq>` (T1, root).
//!
//! A reseal acknowledges the **outstanding** break (the first one no
//! earlier reseal acknowledged): `ackSeq` must name it, so an operator
//! can't wave through a break they haven't looked at. It appends
//! `audit.resealed {broken_at_seq, prior_head}` after the DB head — plus the
//! break's evidence (`class`, `fingerprint`) and the `audit.chain_broken`
//! row that records it (`chain_broken {seq, hash}`), so it acknowledges
//! exactly that break and nothing that later shows up at the same seq
//! (review B-1) — and re-walks the chain: `degraded` clears unless another
//! unacknowledged break remains (then that one is outstanding). The break itself stays in
//! the chain for ever; every later walk sees it, acknowledged.
//!
//! A T1 broker that is degraded notices a root-CLI reseal on its next
//! append (`Chain::append` step 0) and resumes from the reseal row.

use serde_json::{json, Value};
use sqlx::Connection;

use super::chain::{broken_event, db_head, insert_row, Broken, Head};
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
/// columns set; this adds `{broken_at_seq, reason, class, fingerprint,
/// chain_broken, prior_head}`.
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
    let mut prior = db_head(&mut tx).await.map_err(AccessError::internal)?;
    // The acknowledgment names the `audit.chain_broken` row that records
    // this break (review B-1); `verify_boot` above wrote it, but write it
    // here if another writer's walk raced us.
    let recorded_by = match verdict.recorded_by {
        Some(h) => h,
        None => {
            let h = insert_row(&mut tx, chain.store_id(), prior, &broken_event(&b))
                .await
                .map_err(AccessError::internal)?;
            prior = Some(h);
            h
        }
    };
    // Bound to this break's evidence, not just its seq (review B-1): a
    // different break at the same seq later is outstanding again.
    let ev = ev.detail(json!({
        "broken_at_seq": ack_seq,
        "reason": b.reason,
        "class": b.class,
        "fingerprint": b.fingerprint,
        "chain_broken": { "seq": recorded_by.seq, "hash": hex::encode(recorded_by.hash) },
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

    async fn add_rows(store: &AccessStore, n: usize) {
        for i in 0..n {
            crate::access::audit::record(
                store,
                &Event::new(
                    "permission.decided",
                    crate::access::audit::AuditVia::Operator,
                )
                .target(format!("Read f{i}")),
            )
            .await
            .unwrap();
        }
    }

    async fn walk(store: &AccessStore) -> verify_boot::Verdict {
        let mut conn = store.pool().acquire().await.unwrap();
        verify_boot::verify(&mut conn, &store.meta().store_id)
            .await
            .unwrap()
    }

    async fn sql(store: &AccessStore, q: &str) {
        sqlx::raw_sql(q).execute(store.pool()).await.unwrap();
    }

    fn cli_ack() -> Event {
        Event::new("audit.resealed", crate::access::audit::AuditVia::Cli)
    }

    /// Break #3 and reseal it; returns the `audit.chain_broken` seq.
    async fn broken_and_resealed(store: &AccessStore) -> i64 {
        add_rows(store, 5).await;
        forge(store, 3).await;
        let r = reseal(store, 3, cli_ack()).await.unwrap();
        assert!(r.still_broken.is_none());
        assert!(walk(store).await.ok());
        sqlx::query_scalar("SELECT seq FROM audit_events WHERE kind = 'audit.chain_broken'")
            .fetch_one(store.pool())
            .await
            .unwrap()
    }

    /// Review B-1: a reseal acknowledges the break it saw, not its seq.
    /// Deleting the rows from the break through the `audit.chain_broken`
    /// row (the reviewer's repro), deleting just that row, or re-forging
    /// the broken row are new, outstanding breaks — and a restart degrades.
    #[tokio::test]
    async fn a_reseal_acknowledges_only_the_break_it_saw() {
        // The repro: rows 1..6, forge #3, reseal, DELETE #3..#chain_broken.
        let store = AccessStore::memory_t0().await;
        let cb = broken_and_resealed(&store).await;
        // The export says the chain verifies *with* the break resealed.
        let built = crate::access::audit::export::build(
            &store,
            &crate::access::audit::list::Filter::default(),
            &crate::access::audit::list::Visibility::All,
            None,
        )
        .await
        .unwrap();
        assert_eq!(built.manifest["verified"], true);
        assert_eq!(built.manifest["resealed"], json!([3]));
        sql(
            &store,
            &format!("DELETE FROM audit_events WHERE seq BETWEEN 3 AND {cb}"),
        )
        .await;
        let v = walk(&store).await;
        let b = v.outstanding.expect("a grown gap is a new break");
        assert_eq!((b.broken_at_seq, b.class.as_str()), (3, "gap"));
        let restarted = store.clone_with_fresh_chain();
        {
            let mut conn = store.pool().acquire().await.unwrap();
            restarted.chain().verify_boot(&mut conn).await.unwrap();
        }
        assert_eq!(restarted.chain().degraded().unwrap().broken_at_seq, 3);
        let built = crate::access::audit::export::build(
            &restarted,
            &crate::access::audit::list::Filter::default(),
            &crate::access::audit::list::Visibility::All,
            None,
        )
        .await
        .unwrap();
        assert_eq!(built.manifest["verified"], false);
        assert_eq!(built.manifest["contiguous"], false);

        // Only the chain_broken row deleted: a gap at its seq.
        let store = AccessStore::memory_t0().await;
        let cb = broken_and_resealed(&store).await;
        sql(
            &store,
            &format!("DELETE FROM audit_events WHERE seq = {cb}"),
        )
        .await;
        let v = walk(&store).await;
        // The reseal named that row, so it no longer acknowledges #3, and
        // the deletion is a break of its own.
        assert_eq!(v.outstanding.expect("a deleted record").broken_at_seq, 3);
        assert!(v
            .breaks
            .iter()
            .any(|(b, acked)| b.broken_at_seq == cb && b.class == "gap" && !acked));

        // #3 re-forged after the reseal: the same seq, different evidence.
        let store = AccessStore::memory_t0().await;
        broken_and_resealed(&store).await;
        sql(
            &store,
            "UPDATE audit_events SET target = 'forged again' WHERE seq = 3",
        )
        .await;
        let v = walk(&store).await;
        let b = v.outstanding.expect("a re-forged row is a new break");
        assert_eq!((b.broken_at_seq, b.class.as_str()), (3, "hash"));
        assert!(
            !v.recorded,
            "the old chain_broken row records the old evidence"
        );
        // An operator who reviews it can reseal it; the first reseal still
        // doesn't count for it.
        reseal(&store, 3, cli_ack()).await.unwrap();
        let v = walk(&store).await;
        assert!(v.ok(), "{v:?}");
        let rows: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE kind = 'audit.chain_broken'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(rows, 2, "each break is recorded once");
    }

    /// Review M-2: a break found by an append that is then refused is
    /// persisted at once — not only in memory — so a tail truncation
    /// survives the process going away.
    #[tokio::test]
    async fn a_break_found_by_a_refused_append_survives_a_restart() {
        let store = AccessStore::memory_t0().await;
        add_rows(&store, 3).await;
        let head = store.chain().known_head().unwrap();
        assert_eq!(head.seq, 4);
        // Truncate the tail this process wrote.
        sql(
            &store,
            "DROP TRIGGER audit_events_no_delete; DELETE FROM audit_events WHERE seq = 4;",
        )
        .await;
        {
            let mut conn = store.pool().acquire().await.unwrap();
            let mut tx = conn.begin_with("BEGIN IMMEDIATE").await.unwrap();
            let refused = store
                .chain()
                .append(
                    &mut tx,
                    &Event::new(
                        "device.tier_changed",
                        crate::access::audit::AuditVia::Operator,
                    ),
                )
                .await;
            assert!(matches!(
                refused,
                Err(crate::access::audit::chain::AppendError::AuditUnavailable(
                    _
                ))
            ));
            // The caller rolls back.
        }
        // The chain_broken row lands on its own, without another append.
        let mut written = false;
        for _ in 0..200 {
            let n: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM audit_events WHERE kind = 'audit.chain_broken'",
            )
            .fetch_one(store.pool())
            .await
            .unwrap();
            if n == 1 {
                written = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(written, "the break was only in memory");
        // "Restart": a fresh chain view boots degraded at the same seq.
        let restarted = store.clone_with_fresh_chain();
        {
            let mut conn = store.pool().acquire().await.unwrap();
            restarted.chain().verify_boot(&mut conn).await.unwrap();
        }
        assert_eq!(restarted.chain().degraded().unwrap().broken_at_seq, 4);
    }

    async fn chain_broken_rows(store: &AccessStore) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE kind = 'audit.chain_broken'")
            .fetch_one(store.pool())
            .await
            .unwrap()
    }

    /// Poll until the chain holds `n` `audit.chain_broken` rows.
    async fn wait_for_chain_broken(store: &AccessStore, n: i64) {
        for _ in 0..200 {
            if chain_broken_rows(store).await >= n {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the break was only in memory");
    }

    /// An access change through `store`'s chain, which a degraded chain
    /// refuses (the caller rolls back).
    async fn refused_access_change(store: &AccessStore) {
        let mut conn = store.pool().acquire().await.unwrap();
        let mut tx = conn.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let refused = store
            .chain()
            .append(
                &mut tx,
                &Event::new(
                    "device.tier_changed",
                    crate::access::audit::AuditVia::Operator,
                ),
            )
            .await;
        assert!(
            matches!(
                refused,
                Err(crate::access::audit::chain::AppendError::AuditUnavailable(
                    _
                ))
            ),
            "{refused:?}"
        );
    }

    async fn boot(store: &AccessStore) -> AccessStore {
        let restarted = store.clone_with_fresh_chain();
        let mut conn = store.pool().acquire().await.unwrap();
        restarted.chain().verify_boot(&mut conn).await.unwrap();
        drop(conn);
        restarted
    }

    async fn reverify(store: &AccessStore) {
        let mut conn = store.pool().acquire().await.unwrap();
        store.chain().verify_boot(&mut conn).await.unwrap();
    }

    async fn export_all(store: &AccessStore) -> crate::access::audit::export::Built {
        crate::access::audit::export::build(
            store,
            &crate::access::audit::list::Filter::default(),
            &crate::access::audit::list::Visibility::All,
            None,
        )
        .await
        .unwrap()
    }

    /// Review M-3: a tail truncation of two or more rows, found by an
    /// access change that is then refused, is recorded at the first missing
    /// seq and survives the walk — a restart boots degraded, and the same
    /// process stays degraded across `verify_boot` and an export.
    #[tokio::test]
    async fn a_multi_row_tail_truncation_found_by_a_refused_append_stays_degraded() {
        let store = AccessStore::memory_t0().await;
        add_rows(&store, 4).await;
        assert_eq!(store.chain().known_head().unwrap().seq, 5);
        sql(
            &store,
            "DROP TRIGGER audit_events_no_delete; DELETE FROM audit_events WHERE seq >= 4;",
        )
        .await;
        refused_access_change(&store).await;
        wait_for_chain_broken(&store, 1).await;
        let (seq, detail): (i64, String) = sqlx::query_as(
            "SELECT seq, detail FROM audit_events WHERE kind = 'audit.chain_broken'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        let detail: Value = serde_json::from_str(&detail).unwrap();
        // The first missing seq — the seq the recording itself takes — with
        // the lost head (#5) as the evidence.
        assert_eq!(seq, 4);
        assert_eq!(detail["broken_at_seq"], 4);
        assert_eq!(detail["class"], "head_missing");
        assert!(detail["fingerprint"].as_str().unwrap().starts_with("#5:"));

        let v = walk(&store).await;
        let b = v
            .outstanding
            .clone()
            .expect("the recording survives the walk");
        assert_eq!((b.broken_at_seq, b.class.as_str()), (4, "recorded"));
        assert!(v.recorded);

        // (a) A restart boots degraded.
        let restarted = boot(&store).await;
        assert_eq!(restarted.status(), ("degraded", Some(4)));
        // (b) The same process stays degraded after a verify and an export.
        reverify(&store).await;
        assert_eq!(store.status(), ("degraded", Some(4)));
        let built = export_all(&store).await;
        assert_eq!(built.manifest["verified"], false);
        assert_eq!(built.manifest["broken_at_seq"], 4);
        assert_eq!(store.status(), ("degraded", Some(4)));
        refused_access_change(&store).await;
        assert_eq!(chain_broken_rows(&store).await, 1, "recorded once");

        // B-1 still holds for a head break: reseal it, then delete the
        // recording (rows after it remain) — a new, outstanding gap.
        let r = reseal(&store, 4, cli_ack()).await.unwrap();
        assert!(r.still_broken.is_none(), "{r:?}");
        assert_eq!(store.status(), ("ok", None));
        assert!(boot(&store).await.chain().degraded().is_none());
        sql(&store, "DELETE FROM audit_events WHERE seq = 4").await;
        let v = walk(&store).await;
        let b = v.outstanding.expect("a deleted recording is a new break");
        assert_eq!((b.broken_at_seq, b.class.as_str()), (4, "gap"));
        assert_eq!(boot(&store).await.status(), ("degraded", Some(4)));

        // ...or re-forge it: a new, outstanding break at its seq.
        let store = AccessStore::memory_t0().await;
        add_rows(&store, 4).await;
        sql(
            &store,
            "DROP TRIGGER audit_events_no_delete; DELETE FROM audit_events WHERE seq >= 4;",
        )
        .await;
        refused_access_change(&store).await;
        wait_for_chain_broken(&store, 1).await;
        reseal(&store, 4, cli_ack()).await.unwrap();
        assert!(walk(&store).await.ok());
        sql(
            &store,
            "DROP TRIGGER audit_events_no_update; \
             UPDATE audit_events SET target = 'forged' WHERE seq = 4;",
        )
        .await;
        let v = walk(&store).await;
        let b = v.outstanding.expect("a re-forged recording is a new break");
        assert_eq!((b.broken_at_seq, b.class.as_str()), (4, "hash"));
        assert_eq!(boot(&store).await.status(), ("degraded", Some(4)));
    }

    /// Review M-3: the allowed-append path (an authentication row after a
    /// two-row truncation) records the break in the caller's transaction
    /// and stays degraded across a restart; a rollback doesn't lose it.
    #[tokio::test]
    async fn an_allowed_append_after_a_multi_row_truncation_stays_degraded() {
        let store = AccessStore::memory_t0().await;
        add_rows(&store, 4).await;
        sql(
            &store,
            "DROP TRIGGER audit_events_no_delete; DELETE FROM audit_events WHERE seq >= 4;",
        )
        .await;
        // A rolled-back authentication row first: the break stays queued.
        {
            let mut conn = store.pool().acquire().await.unwrap();
            let mut tx = conn.begin_with("BEGIN IMMEDIATE").await.unwrap();
            store
                .chain()
                .append(
                    &mut tx,
                    &Event::new("auth.login_ok", crate::access::audit::AuditVia::Session),
                )
                .await
                .unwrap();
            tx.rollback().await.unwrap();
        }
        assert_eq!(store.status(), ("degraded", Some(4)));
        let head = crate::access::audit::record(
            &store,
            &Event::new("auth.login_ok", crate::access::audit::AuditVia::Session),
        )
        .await
        .unwrap();
        assert_eq!(head.seq, 5, "the recording at #4, the login at #5");
        wait_for_chain_broken(&store, 1).await;

        assert_eq!(boot(&store).await.status(), ("degraded", Some(4)));
        reverify(&store).await;
        assert_eq!(store.status(), ("degraded", Some(4)));
        assert_eq!(export_all(&store).await.manifest["verified"], false);
        assert_eq!(store.status(), ("degraded", Some(4)));
        // The flush that confirms the in-transaction row wrote no second one.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        reverify(&store).await;
        assert_eq!(chain_broken_rows(&store).await, 1);
    }

    /// Review M-3 re-audit: a head break isn't dropped when its recording
    /// lands before the seq it names (the tail shrank again before the row
    /// was flushed), nor shadowed by a different break the walk finds at
    /// that seq — resealing that one leaves the head break outstanding.
    #[tokio::test]
    async fn a_head_break_is_neither_dropped_nor_shadowed() {
        let store = AccessStore::memory_t0().await;
        add_rows(&store, 4).await;
        // The "process": a chain view with no pool attached, so a refused
        // append leaves its break queued (as a busy store would).
        let p = store.clone_with_fresh_chain();
        reverify(&p).await;
        assert_eq!(p.chain().known_head().unwrap().seq, 5);
        sql(
            &store,
            "DROP TRIGGER audit_events_no_update; DROP TRIGGER audit_events_no_delete; \
             DELETE FROM audit_events WHERE seq >= 4;",
        )
        .await;
        refused_access_change(&p).await;
        // The tail shrinks again before the recording is written.
        sql(&store, "DELETE FROM audit_events WHERE seq = 3").await;
        reverify(&p).await;
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT seq, detail FROM audit_events WHERE kind = 'audit.chain_broken' ORDER BY seq",
        )
        .fetch_all(store.pool())
        .await
        .unwrap();
        // #3 records the break at #4 (it names a seq after itself) and #4
        // records that the re-based head #3 changed.
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].0, 3);
        assert!(rows[0].1.contains("\"broken_at_seq\":4"), "{rows:?}");
        assert!(rows[0].1.contains("head_missing"), "{rows:?}");
        assert!(rows[1].1.contains("head_changed"), "{rows:?}");
        let v = walk(&store).await;
        let at: Vec<(i64, &str)> = v
            .breaks
            .iter()
            .map(|(b, _)| (b.broken_at_seq, b.class.as_str()))
            .collect();
        assert_eq!(at, [(3, "recorded"), (4, "recorded")]);
        assert_eq!(boot(&store).await.status(), ("degraded", Some(3)));

        // Forge #4 (the head_changed recording): the walk finds a hash
        // break at #4, which must not swallow #3's head break at #4.
        sql(
            &store,
            "UPDATE audit_events SET target = 'forged' WHERE seq = 4",
        )
        .await;
        let v = walk(&store).await;
        let at: Vec<(i64, &str)> = v
            .breaks
            .iter()
            .map(|(b, _)| (b.broken_at_seq, b.class.as_str()))
            .collect();
        assert_eq!(at, [(4, "hash"), (4, "recorded")]);
        let r = reseal(&p, 4, cli_ack()).await.unwrap();
        let still = r.still_broken.expect("the head break stays outstanding");
        assert_eq!((still.broken_at_seq, still.class.as_str()), (4, "recorded"));
        assert_eq!(boot(&store).await.status(), ("degraded", Some(4)));
        let r = reseal(&p, 4, cli_ack()).await.unwrap();
        assert!(r.still_broken.is_none(), "{r:?}");
        assert!(walk(&store).await.ok());
    }

    /// Review M-3 re-audit: a tail truncation while the store is degraded
    /// for another break is not lost when another process reseals that
    /// break — the known head is checked before a clean verdict is adopted
    /// (step 0 of an append, and `verify_boot`).
    #[tokio::test]
    async fn a_truncation_while_degraded_survives_a_reseal_of_the_other_break() {
        let store = AccessStore::memory_t0().await;
        add_rows(&store, 5).await;
        forge(&store, 2).await;
        reverify(&store).await;
        assert_eq!(store.status(), ("degraded", Some(2)));
        let known = store.chain().known_head().unwrap();
        assert_eq!(known.seq, 7, "the chain_broken row");
        // Truncate the tail (the recording with it), then the "CLI"
        // reseals the forged row.
        sql(&store, "DELETE FROM audit_events WHERE seq >= 6").await;
        let cli = store.clone_with_fresh_chain();
        let r = reseal(&cli, 2, cli_ack()).await.unwrap();
        assert!(r.still_broken.is_none(), "{r:?}");
        // The running process's next access change sees the reseal and the
        // regression; it stays degraded instead of adopting the clean walk.
        refused_access_change(&store).await;
        let b = store.chain().degraded().unwrap();
        assert_eq!((b.broken_at_seq, b.class.as_str()), (7, "head_changed"));
        reverify(&store).await;
        assert_eq!(store.status(), ("degraded", Some(7)));
        assert_eq!(boot(&store).await.status(), ("degraded", Some(7)));
    }
}
