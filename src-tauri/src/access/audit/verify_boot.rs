//! The reseal-aware full verification (G-ACCESS §6.4, WP-77): what runs at
//! every start of the T0 daemon and the T1 broker, on
//! `access_audit_verify`, before every export, and in
//! `ikenga-server audit verify`.
//!
//! [`super::chain::verify_all`] is the strict walk (A-16: it stops at the
//! first row that doesn't link or hash). A store that was broken and then
//! resealed keeps its break **for ever** (§6.4 "The break stays in the
//! chain"), so the boot walk can't simply stop there or the store would
//! degrade again on every restart. This walk therefore:
//!
//! * re-anchors past each break (on the stored `prev_hash` of the next row
//!   after a gap or bad link, and on the stored `hash` of a row whose
//!   contents no longer match) so the rest of the chain is still checked;
//! * collects the `audit.chain_broken {broken_at_seq}` rows the chain
//!   records and the `audit.resealed {broken_at_seq}` rows that acknowledge
//!   them (a reseal counts only when it comes after the break);
//! * reports the first break — found by the walk or recorded by a running
//!   process (a head regression the walk can't see, §6.4 "Tail
//!   truncation") — that no reseal acknowledges. That one keeps the store
//!   `degraded` across restarts until an operator reseals it.

use std::collections::{BTreeMap, BTreeSet};

use sqlx::SqliteConnection;

use super::chain::{genesis, to32, Broken, Head, StoredRow, VerifyReport, COLUMNS};

/// The outcome of the reseal-aware walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// Rows walked.
    pub rows: i64,
    /// The newest row (by its stored hash: what the next append links to).
    pub head: Option<Head>,
    /// Every break the walk found, in `seq` order, and whether a later
    /// `audit.resealed` row acknowledges it.
    pub breaks: Vec<(Broken, bool)>,
    /// The first unacknowledged break, if any: the store is `degraded`.
    pub outstanding: Option<Broken>,
    /// Whether the chain already holds an `audit.chain_broken` row for
    /// [`outstanding`](Self::outstanding).
    pub recorded: bool,
    /// `broken_at_seq`s acknowledged by a reseal.
    pub resealed: Vec<i64>,
}

impl Verdict {
    pub fn ok(&self) -> bool {
        self.outstanding.is_none()
    }

    pub fn report(&self) -> VerifyReport {
        VerifyReport {
            rows: self.rows,
            head: self.head,
            broken: self.outstanding.clone(),
        }
    }
}

fn detail_seq(detail: &str) -> Option<i64> {
    serde_json::from_str::<serde_json::Value>(detail)
        .ok()?
        .get("broken_at_seq")?
        .as_i64()
}

fn detail_reason(detail: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(detail)
        .ok()?
        .get("reason")?
        .as_str()
        .map(str::to_string)
}

/// The full, reseal-aware walk from genesis. Read-only.
pub async fn verify(conn: &mut SqliteConnection, store_id: &str) -> Result<Verdict, sqlx::Error> {
    let rows = sqlx::query(&format!("SELECT {COLUMNS} FROM audit_events ORDER BY seq"))
        .fetch_all(&mut *conn)
        .await?;
    let mut prev = genesis(store_id);
    let mut expected = 1i64;
    let mut count = 0i64;
    let mut head: Option<Head> = None;
    let mut found: Vec<Broken> = Vec::new();
    // broken_at_seq → the recording row's reason.
    let mut recorded: BTreeMap<i64, String> = BTreeMap::new();
    // broken_at_seqs a later reseal acknowledges.
    let mut acked: BTreeSet<i64> = BTreeSet::new();
    for r in &rows {
        let row = StoredRow::from_sql(r)?;
        let mut row_ok = true;
        if row.seq != expected {
            found.push(Broken {
                broken_at_seq: expected,
                reason: format!(
                    "seq gap: expected #{expected}, found #{} (rows deleted)",
                    row.seq
                ),
            });
            // Re-anchor on what the next surviving row says came before it.
            if let Some(p) = to32(&row.prev_hash) {
                prev = p;
            }
        }
        if row.prev_hash.as_slice() != prev.as_slice() {
            found.push(Broken {
                broken_at_seq: row.seq,
                reason: "prev_hash does not link to the previous row".into(),
            });
            row_ok = false;
            if let Some(p) = to32(&row.prev_hash) {
                prev = p;
            }
        }
        let computed = row.compute_hash(&prev);
        if row.hash.as_slice() != computed.as_slice() {
            found.push(Broken {
                broken_at_seq: row.seq,
                reason: "row hash does not match its contents".into(),
            });
            row_ok = false;
        }
        if row.seq == 1 && row.kind != "store.created" {
            found.push(Broken {
                broken_at_seq: 1,
                reason: "row 1 is not store.created".into(),
            });
            row_ok = false;
        }
        // Only rows that verify may record or acknowledge a break.
        if row_ok {
            match row.kind.as_str() {
                "audit.chain_broken" => {
                    if let Some(at) = detail_seq(&row.detail) {
                        recorded.entry(at).or_insert_with(|| {
                            detail_reason(&row.detail).unwrap_or_else(|| "recorded break".into())
                        });
                    }
                }
                "audit.resealed" => {
                    if let Some(at) = detail_seq(&row.detail).filter(|at| *at < row.seq) {
                        acked.insert(at);
                    }
                }
                _ => {}
            }
        }
        // The next row links to the hash as stored.
        prev = to32(&row.hash).unwrap_or(computed);
        head = Some(Head {
            seq: row.seq,
            hash: prev,
        });
        expected = row.seq + 1;
        count += 1;
    }
    if rows.is_empty() {
        let migrated: i64 = sqlx::query_scalar("SELECT count(*) FROM store_meta")
            .fetch_one(&mut *conn)
            .await?;
        if migrated > 0 {
            found.push(Broken {
                broken_at_seq: 1,
                reason: "row 1 (store.created) is missing: the chain is empty".into(),
            });
        }
    }
    // Dedupe breaks by seq (a gap and a link failure can name the same row).
    let mut by_seq: BTreeMap<i64, Broken> = BTreeMap::new();
    for b in found {
        by_seq.entry(b.broken_at_seq).or_insert(b);
    }
    // Breaks only a running process saw (a head regression) are recorded,
    // not found by the walk.
    for (at, reason) in &recorded {
        by_seq.entry(*at).or_insert_with(|| Broken {
            broken_at_seq: *at,
            reason: reason.clone(),
        });
    }
    let breaks: Vec<(Broken, bool)> = by_seq
        .into_values()
        .map(|b| {
            let ack = acked.contains(&b.broken_at_seq);
            (b, ack)
        })
        .collect();
    let outstanding = breaks.iter().find(|(_, ack)| !ack).map(|(b, _)| b.clone());
    let is_recorded = outstanding
        .as_ref()
        .is_some_and(|b| recorded.contains_key(&b.broken_at_seq));
    Ok(Verdict {
        rows: count,
        head,
        breaks,
        outstanding,
        recorded: is_recorded,
        resealed: acked.into_iter().collect(),
    })
}

// ── `ikenga-server audit …` (the root CLI, §6.4, §6.8) ──────────────────────

/// The store `ikenga-server audit` works on: `--file <db>` as named, else
/// the T1 operator store under `--data-dir` (`operator/accounts.db`), which
/// only root may open.
pub fn cli_store_path(
    data_dir: Option<std::path::PathBuf>,
    file: Option<std::path::PathBuf>,
) -> anyhow::Result<std::path::PathBuf> {
    if let Some(f) = file {
        return Ok(f);
    }
    #[cfg(target_os = "linux")]
    {
        let dir = data_dir.ok_or_else(|| {
            anyhow::anyhow!(
                "`audit` needs --data-dir (or IKENGA_DATA_DIR, the T1 operator root) or --file <db>"
            )
        })?;
        let dir = if dir.is_absolute() {
            dir
        } else {
            std::env::current_dir()?.join(dir)
        };
        // SAFETY: geteuid has no preconditions and cannot fail.
        if unsafe { libc::geteuid() } != 0 {
            anyhow::bail!(
                "`ikenga-server audit` on the T1 operator store must run as root (it is \
                 operator-wide); name a store you own with --file instead"
            );
        }
        Ok(crate::server::operator::OperatorRoot::new(dir)?.accounts_db())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = data_dir;
        anyhow::bail!(
            "name the access store with --file <db> (the T1 operator store is Linux-only)"
        )
    }
}

/// `ikenga-server audit verify`: the reseal-aware walk, read-only (it
/// neither records `audit.verified` nor degrades anything — the running
/// server does that at its next start or verify). Returns the report text
/// and whether the chain holds.
pub async fn cli_verify(
    store: &crate::access::AccessStore,
    json: bool,
) -> anyhow::Result<(String, bool)> {
    let mut conn = store.pool().acquire().await?;
    let v = verify(&mut conn, &store.meta().store_id).await?;
    let head_hash = v.head.map(|h| hex::encode(h.hash));
    let text = if json {
        serde_json::json!({
            "ok": v.ok(),
            "store_id": store.meta().store_id,
            "tier": store.meta().tier.as_str(),
            "rows": v.rows,
            "head_seq": v.head.map(|h| h.seq),
            "head_hash": head_hash,
            "broken_at_seq": v.outstanding.as_ref().map(|b| b.broken_at_seq),
            "reason": v.outstanding.as_ref().map(|b| b.reason.clone()),
            "resealed": v.resealed,
        })
        .to_string()
    } else {
        let mut t = format!(
            "audit chain ({} store {}): {} rows, head #{} {}\n",
            store.meta().tier.as_str(),
            store.meta().store_id,
            v.rows,
            v.head.map(|h| h.seq).unwrap_or(0),
            head_hash.as_deref().unwrap_or("-")
        );
        for (b, acked) in &v.breaks {
            t.push_str(&format!(
                "  break at #{}: {}{}\n",
                b.broken_at_seq,
                b.reason,
                if *acked { " (resealed)" } else { "" }
            ));
        }
        match &v.outstanding {
            None => t.push_str("OK: the chain verifies\n"),
            Some(b) => t.push_str(&format!(
                "BROKEN at #{}: access changes are paused until an operator runs \
                 `ikenga-server audit reseal --ack {}`\n",
                b.broken_at_seq, b.broken_at_seq
            )),
        }
        t
    };
    Ok((text, v.ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::audit::chain::Chain;
    use crate::access::audit::{AuditVia, Event};
    use sqlx::Connection;

    async fn store() -> SqliteConnection {
        let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/0001_core.sql"))
            .execute(&mut conn)
            .await
            .unwrap();
        conn
    }

    async fn append(conn: &mut SqliteConnection, chain: &Chain, ev: Event) {
        let mut tx = conn.begin().await.unwrap();
        let h = chain.append(&mut tx, &ev).await.unwrap();
        tx.commit().await.unwrap();
        chain.committed(h);
    }

    async fn seed(conn: &mut SqliteConnection, chain: &Chain, n: usize) {
        append(conn, chain, Event::new("store.created", AuditVia::System)).await;
        for i in 0..n {
            append(
                conn,
                chain,
                Event::new("device.tier_changed", AuditVia::Operator).target(format!("d{i}")),
            )
            .await;
        }
    }

    async fn untrigger(conn: &mut SqliteConnection) {
        sqlx::raw_sql("DROP TRIGGER audit_events_no_update; DROP TRIGGER audit_events_no_delete;")
            .execute(&mut *conn)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_clean_chain_has_no_breaks() {
        let mut conn = store().await;
        let chain = Chain::new("s");
        seed(&mut conn, &chain, 5).await;
        let v = verify(&mut conn, "s").await.unwrap();
        assert!(v.ok(), "{v:?}");
        assert_eq!(v.rows, 6);
        assert_eq!(v.head, chain.known_head());
    }

    /// The walk keeps going past a forged row and reports every break.
    #[tokio::test]
    async fn the_walk_reanchors_and_finds_every_break() {
        let mut conn = store().await;
        let chain = Chain::new("s");
        seed(&mut conn, &chain, 6).await;
        untrigger(&mut conn).await;
        sqlx::query("UPDATE audit_events SET target = 'forged' WHERE seq IN (2, 5)")
            .execute(&mut conn)
            .await
            .unwrap();
        let v = verify(&mut conn, "s").await.unwrap();
        let at: Vec<i64> = v.breaks.iter().map(|(b, _)| b.broken_at_seq).collect();
        assert_eq!(at, [2, 5]);
        assert_eq!(v.outstanding.unwrap().broken_at_seq, 2);
        assert!(!v.recorded);
        // A middle deletion is one break, at the missing seq.
        let mut conn = store().await;
        seed(&mut conn, &Chain::new("s"), 6).await;
        untrigger(&mut conn).await;
        sqlx::query("DELETE FROM audit_events WHERE seq = 4")
            .execute(&mut conn)
            .await
            .unwrap();
        let v = verify(&mut conn, "s").await.unwrap();
        let at: Vec<i64> = v.breaks.iter().map(|(b, _)| b.broken_at_seq).collect();
        assert_eq!(at, [4]);
    }

    /// Boot records a break once, stays degraded across restarts, and a
    /// reseal (a later `audit.resealed`) clears it for good while the break
    /// stays in the chain.
    #[tokio::test]
    async fn a_recorded_break_persists_until_resealed() {
        let mut conn = store().await;
        let chain = Chain::new("s");
        seed(&mut conn, &chain, 3).await;
        untrigger(&mut conn).await;
        sqlx::query("UPDATE audit_events SET target = 'forged' WHERE seq = 2")
            .execute(&mut conn)
            .await
            .unwrap();

        let first = Chain::new("s");
        assert_eq!(
            first
                .verify_boot(&mut conn)
                .await
                .unwrap()
                .broken
                .unwrap()
                .broken_at_seq,
            2
        );
        let second = Chain::new("s");
        second.verify_boot(&mut conn).await.unwrap();
        assert_eq!(second.degraded().unwrap().broken_at_seq, 2);
        let rows: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE kind = 'audit.chain_broken'",
        )
        .fetch_one(&mut conn)
        .await
        .unwrap();
        assert_eq!(rows, 1, "a restart doesn't record the same break twice");

        append(
            &mut conn,
            &second,
            Event::new("audit.resealed", AuditVia::Operator)
                .detail(serde_json::json!({"broken_at_seq": 2})),
        )
        .await;
        let third = Chain::new("s");
        let r = third.verify_boot(&mut conn).await.unwrap();
        assert!(r.ok(), "{r:?}");
        assert!(third.degraded().is_none());
        let v = verify(&mut conn, "s").await.unwrap();
        assert_eq!(v.resealed, [2]);
        assert_eq!(v.breaks.len(), 1, "the break stays in the chain");
    }

    /// `ikenga-server audit verify | export | reseal` over a store file
    /// (the functions `main.rs` calls): a second writer next to nothing.
    #[tokio::test]
    async fn the_cli_verifies_exports_and_reseals_a_store_file() {
        use crate::access::audit::{export, list::Filter, reseal};
        use crate::access::{AccessStore, HostIdentity};
        let tmp = tempfile::tempdir().unwrap();
        let host = HostIdentity {
            username: "ned".into(),
            hostname: "h".into(),
        };
        AccessStore::open_t0(tmp.path(), &host)
            .await
            .unwrap()
            .pool()
            .close()
            .await;
        let db = AccessStore::t0_path(tmp.path());
        let path = cli_store_path(None, Some(db.clone())).unwrap();
        assert_eq!(path, db);
        assert!(AccessStore::open_cli(&tmp.path().join("nope.db"))
            .await
            .is_err());

        let store = AccessStore::open_cli(&path).await.unwrap();
        let (text, ok) = cli_verify(&store, false).await.unwrap();
        assert!(ok, "{text}");
        let (json, _) = cli_verify(&store, true).await.unwrap();
        let json: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(json["tier"], "t0");
        assert_eq!(json["rows"], 1);

        let out = tmp.path().join("x.jsonl");
        let filter = Filter::from_parts(None, None, Some("access".into()), None, None).unwrap();
        assert!(Filter::from_parts(None, None, Some("nope".into()), None, None).is_err());
        let built = export::export_cli(&store, &filter, Some(&out))
            .await
            .unwrap();
        assert_eq!(built.rows, 1);
        assert!(std::fs::read_to_string(&out)
            .unwrap()
            .contains("store.created"));

        let ack = || Event::new("audit.resealed", AuditVia::Cli);
        assert!(
            reseal::reseal(&store, 1, ack()).await.is_err(),
            "nothing to reseal"
        );
        sqlx::raw_sql(
            "DROP TRIGGER audit_events_no_update; \
             UPDATE audit_events SET target = 'forged' WHERE seq = 1;",
        )
        .execute(store.pool())
        .await
        .unwrap();
        let (text, ok) = cli_verify(&store, false).await.unwrap();
        assert!(!ok);
        assert!(text.contains("BROKEN at #1"), "{text}");
        let r = reseal::reseal(&store, 1, ack()).await.unwrap();
        assert!(r.still_broken.is_none());
        let (text, ok) = cli_verify(&store, false).await.unwrap();
        assert!(ok, "{text}");
        assert!(text.contains("(resealed)"), "{text}");
    }

    /// A head regression is invisible to the walk; the running process's
    /// recorded `audit.chain_broken` keeps it degraded after a restart.
    #[tokio::test]
    async fn a_recorded_head_regression_survives_a_restart() {
        let mut conn = store().await;
        let chain = Chain::new("s");
        seed(&mut conn, &chain, 3).await;
        untrigger(&mut conn).await;
        sqlx::query("DELETE FROM audit_events WHERE seq = 4")
            .execute(&mut conn)
            .await
            .unwrap();
        // An auth event: allowed while degraded, and records the break.
        append(
            &mut conn,
            &chain,
            Event::new("auth.login_ok", AuditVia::Session),
        )
        .await;
        assert!(
            verify(&mut conn, "s").await.unwrap().breaks[0]
                .0
                .broken_at_seq
                == 4
        );
        let restarted = Chain::new("s");
        restarted.verify_boot(&mut conn).await.unwrap();
        assert_eq!(restarted.degraded().unwrap().broken_at_seq, 4);
    }
}
