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

use super::chain::{
    empty_break, gap_break, genesis, inspect_row, to32, Broken, Head, StoredRow, VerifyReport,
    COLUMNS,
};

/// The outcome of the reseal-aware walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// Rows walked.
    pub rows: i64,
    /// The newest row (by its stored hash: what the next append links to).
    pub head: Option<Head>,
    /// Every break, in `seq` order, and whether a later `audit.resealed`
    /// row acknowledges exactly it (same seq, class and evidence).
    pub breaks: Vec<(Broken, bool)>,
    /// The first unacknowledged break, if any: the store is `degraded`.
    pub outstanding: Option<Broken>,
    /// Whether the chain already holds an `audit.chain_broken` row for
    /// [`outstanding`](Self::outstanding).
    pub recorded: bool,
    /// That `audit.chain_broken` row (a reseal names it).
    pub recorded_by: Option<Head>,
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
            resealed: self.resealed.clone(),
        }
    }
}

fn detail(detail: &str) -> serde_json::Value {
    serde_json::from_str(detail).unwrap_or(serde_json::Value::Null)
}

fn str_of(v: &serde_json::Value, k: &str) -> Option<String> {
    v.get(k)?.as_str().map(str::to_string)
}

/// One verifying `audit.chain_broken` row.
struct Recording {
    at: Head,
    broken_at_seq: i64,
    reason: String,
    /// `None` on a pre-WP-77 row (no evidence): it records "a break at
    /// #seq" for the purpose of not writing a second row, nothing more.
    evidence: Option<(String, String)>,
}

impl Recording {
    fn records(&self, b: &Broken) -> bool {
        self.broken_at_seq == b.broken_at_seq
            && self
                .evidence
                .as_ref()
                .is_none_or(|(c, f)| *c == b.class && *f == b.fingerprint)
    }
}

/// One verifying `audit.resealed` row's acknowledgment: exactly the break
/// `(seq, class, fingerprint)`, recorded by the `audit.chain_broken` row at
/// `recorded_by` (review B-1).
struct Ack {
    broken_at_seq: i64,
    class: String,
    fingerprint: String,
}

fn hash_hex(h: &Head) -> String {
    hex::encode(h.hash)
}

/// The full, reseal-aware walk from genesis. Read-only.
///
/// A reseal acknowledges a break only when its `{broken_at_seq, class,
/// fingerprint}` match the break the walk finds **now** and the
/// `audit.chain_broken` row it names (`chain_broken: {seq, hash}`) is still
/// in the chain and verifies (review B-1). So deleting rows from the break
/// onward (the chain_broken row included), re-forging the broken row, or a
/// gap that grows are all new, outstanding breaks.
pub async fn verify(conn: &mut SqliteConnection, store_id: &str) -> Result<Verdict, sqlx::Error> {
    let rows = sqlx::query(&format!("SELECT {COLUMNS} FROM audit_events ORDER BY seq"))
        .fetch_all(&mut *conn)
        .await?;
    let mut prev = genesis(store_id);
    let mut expected = 1i64;
    let mut count = 0i64;
    let mut head: Option<Head> = None;
    let mut found: Vec<Broken> = Vec::new();
    let mut recordings: Vec<Recording> = Vec::new();
    // (seq, hash hex) of every verifying chain_broken row.
    let mut verified_recordings: BTreeSet<(i64, String)> = BTreeSet::new();
    let mut acks: Vec<Ack> = Vec::new();
    for r in &rows {
        let row = StoredRow::from_sql(r)?;
        if row.seq != expected {
            found.push(gap_break(expected, &row));
            // Re-anchor on what the next surviving row says came before it.
            if let Some(p) = to32(&row.prev_hash) {
                prev = p;
            }
        }
        let row_break = inspect_row(&row, &prev);
        let row_ok = row_break.is_none();
        if let Some(b) = row_break {
            found.push(b);
        }
        let stored = to32(&row.hash).unwrap_or_else(|| row.compute_hash(&prev));
        let at = Head {
            seq: row.seq,
            hash: stored,
        };
        // Only rows that verify may record or acknowledge a break.
        if row_ok {
            let d = detail(&row.detail);
            let seq_of = |k: &str| d.get(k).and_then(serde_json::Value::as_i64);
            match row.kind.as_str() {
                "audit.chain_broken" => {
                    if let Some(b_at) = seq_of("broken_at_seq").filter(|s| *s <= row.seq) {
                        verified_recordings.insert((row.seq, hash_hex(&at)));
                        recordings.push(Recording {
                            at,
                            broken_at_seq: b_at,
                            reason: str_of(&d, "reason").unwrap_or_else(|| "recorded break".into()),
                            evidence: str_of(&d, "class").zip(str_of(&d, "fingerprint")),
                        });
                    }
                }
                "audit.resealed" => {
                    let cb = d.get("chain_broken");
                    let cb_seq = cb
                        .and_then(|c| c.get("seq"))
                        .and_then(serde_json::Value::as_i64);
                    let cb_hash = cb.and_then(|c| str_of(c, "hash"));
                    let named = cb_seq
                        .zip(cb_hash)
                        .is_some_and(|k| k.0 < row.seq && verified_recordings.contains(&k));
                    if let (Some(b_at), Some(class), Some(fp), true) = (
                        seq_of("broken_at_seq").filter(|s| *s < row.seq),
                        str_of(&d, "class"),
                        str_of(&d, "fingerprint"),
                        named,
                    ) {
                        acks.push(Ack {
                            broken_at_seq: b_at,
                            class,
                            fingerprint: fp,
                        });
                    }
                }
                _ => {}
            }
        }
        // The next row links to the hash as stored.
        prev = stored;
        head = Some(at);
        expected = row.seq + 1;
        count += 1;
    }
    if rows.is_empty() {
        let migrated: i64 = sqlx::query_scalar("SELECT count(*) FROM store_meta")
            .fetch_one(&mut *conn)
            .await?;
        if migrated > 0 {
            found.push(empty_break());
        }
    }
    // One walk-found break per seq (a gap and a row break never share one:
    // the gap is at the missing seq, the row break at the row's own).
    let mut by_seq: BTreeMap<i64, Broken> = BTreeMap::new();
    for b in found {
        by_seq.entry(b.broken_at_seq).or_insert(b);
    }
    let mut all: Vec<Broken> = by_seq.values().cloned().collect();
    // Breaks only a running process saw (a head regression, §6.4 "Tail
    // truncation"): each recording of a seq the walk finds clean is its own
    // break, evidenced by the recording row itself.
    for rec in &recordings {
        if !by_seq.contains_key(&rec.broken_at_seq) {
            all.push(Broken::new(
                rec.broken_at_seq,
                rec.reason.clone(),
                "recorded",
                format!("#{}:{}", rec.at.seq, hash_hex(&rec.at)),
            ));
        }
    }
    all.sort_by_key(|b| b.broken_at_seq);
    let breaks: Vec<(Broken, bool)> = all
        .into_iter()
        .map(|b| {
            let ack = acks.iter().any(|a| {
                a.broken_at_seq == b.broken_at_seq
                    && a.class == b.class
                    && a.fingerprint == b.fingerprint
            });
            (b, ack)
        })
        .collect();
    let outstanding = breaks.iter().find(|(_, ack)| !ack).map(|(b, _)| b.clone());
    let recorded_by = outstanding.as_ref().and_then(|b| {
        if b.class == "recorded" {
            // Its evidence is the recording row itself.
            recordings
                .iter()
                .find(|r| format!("#{}:{}", r.at.seq, hash_hex(&r.at)) == b.fingerprint)
                .map(|r| r.at)
        } else {
            recordings.iter().find(|r| r.records(b)).map(|r| r.at)
        }
    });
    let resealed: BTreeSet<i64> = breaks
        .iter()
        .filter(|(_, ack)| *ack)
        .map(|(b, _)| b.broken_at_seq)
        .collect();
    Ok(Verdict {
        rows: count,
        head,
        breaks,
        outstanding,
        recorded: recorded_by.is_some(),
        recorded_by,
        resealed: resealed.into_iter().collect(),
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

/// Review m-8: a warning when root works on a store another user owns (a
/// T0 `access.db`). SQLite gives the `-wal` / `-shm` it creates the
/// database's owner, but whatever `--out` writes stays root's.
pub fn cli_foreign_owner_warning(path: &std::path::Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        let owner = std::fs::metadata(path).ok()?.uid();
        (euid == 0 && owner != 0).then(|| {
            format!(
                "warning: running as root on {} (owned by uid {owner}); prefer running \
                 `ikenga-server audit` as that user",
                path.display()
            )
        })
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
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

        // A seq-only acknowledgment (no evidence) doesn't count (B-1).
        append(
            &mut conn,
            &second,
            Event::new("audit.resealed", AuditVia::Operator)
                .detail(serde_json::json!({"broken_at_seq": 2})),
        )
        .await;
        assert!(!verify(&mut conn, "s").await.unwrap().ok());
        let v = verify(&mut conn, "s").await.unwrap();
        let (b, cb) = (v.outstanding.unwrap(), v.recorded_by.unwrap());
        append(
            &mut conn,
            &second,
            Event::new("audit.resealed", AuditVia::Operator).detail(serde_json::json!({
                "broken_at_seq": 2,
                "class": b.class,
                "fingerprint": b.fingerprint,
                "chain_broken": { "seq": cb.seq, "hash": hex::encode(cb.hash) },
            })),
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
        assert!(AccessStore::open_cli(&tmp.path().join("nope.db"), true)
            .await
            .is_err());
        // `audit verify` opens read-only (review m-8): it can read, not write.
        let ro = AccessStore::open_cli(&path, true).await.unwrap();
        assert!(cli_verify(&ro, false).await.unwrap().1);
        assert!(
            sqlx::query("INSERT INTO store_meta (k, v) VALUES ('x', 'y')")
                .execute(ro.pool())
                .await
                .is_err()
        );
        ro.pool().close().await;

        let store = AccessStore::open_cli(&path, false).await.unwrap();
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
