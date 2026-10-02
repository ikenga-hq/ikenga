//! Absorbing `auth_events` into the chain (G-ACCESS §6.6, §8.3; WP-77) —
//! the body of the `access/0002_absorb_auth_events` Rust step. T1 only,
//! inside the migration batch's one `BEGIN IMMEDIATE`:
//!
//! 1. every existing `auth_events` row is appended to the chain in `id`
//!    order: `kind = 'auth.' || kind`, `at_ms = at * 1000`, `via =
//!    'system'`, `detail = {legacy_id, username_tried, backfilled: true, …}`
//!    (the row's own `detail` keys kept). `seq` is append order, not time
//!    order, so they follow any W3/W4 pairing rows;
//! 2. `auth_events` → `auth_events_legacy`, append-only by trigger; its rows
//!    are kept for ever;
//! 3. `auth_events` becomes a **view** over the chain with G-PRINCIPAL
//!    §6.1's columns, so readers don't change. Not preserved (review C-21,
//!    A-19): `id` is now the chain `seq`, and `detail` gains `legacy_id` /
//!    `backfilled`. A view can't carry §6.1's FK on `principal_id`; that is
//!    enforced in code (A-5). `audit_principal_at` serves it;
//! 4. `server::operator::auth_events::record` sees the view and appends to
//!    the chain from then on (the re-point, R-2) — the broker and the root
//!    CLI alike.

use serde_json::{Map, Value};
use sqlx::{Row, SqliteConnection};

use super::chain::{db_head, insert_row_at};
use super::{static_kind, AuditVia, Event};

/// §6.6 step 3, verbatim.
pub const VIEW_SQL: &str = "CREATE VIEW auth_events AS SELECT seq AS id, at_ms/1000 AS at, \
     principal_id, json_extract(detail,'$.username_tried') AS username_tried, \
     substr(kind,6) AS kind, remote_addr, user_agent, detail \
     FROM audit_events WHERE kind LIKE 'auth.%'";

const LEGACY_TRIGGERS: &str = "\
CREATE TRIGGER auth_events_legacy_no_update BEFORE UPDATE ON auth_events_legacy \
BEGIN SELECT RAISE(ABORT, 'auth_events_legacy is append-only'); END;
CREATE TRIGGER auth_events_legacy_no_delete BEFORE DELETE ON auth_events_legacy \
BEGIN SELECT RAISE(ABORT, 'auth_events_legacy is append-only'); END;";

/// What `auth_events` is in this file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthEventsShape {
    /// No such object (a T0 store).
    Absent,
    /// WP-20's table: not absorbed yet.
    Table,
    /// The §6.6 view: absorbed.
    View,
}

pub async fn shape(conn: &mut SqliteConnection) -> Result<AuthEventsShape, sqlx::Error> {
    let ty: Option<String> =
        sqlx::query_scalar("SELECT type FROM sqlite_master WHERE name = 'auth_events'")
            .fetch_optional(&mut *conn)
            .await?;
    Ok(match ty.as_deref() {
        Some("table") => AuthEventsShape::Table,
        Some("view") => AuthEventsShape::View,
        _ => AuthEventsShape::Absent,
    })
}

/// Run §6.6 steps 1–3 on `conn` (inside the caller's transaction). A
/// no-op when there is no `auth_events` table, or it is already the view.
pub async fn absorb(conn: &mut SqliteConnection) -> anyhow::Result<()> {
    if shape(conn).await? != AuthEventsShape::Table {
        return Ok(());
    }
    let store_id: String = sqlx::query_scalar("SELECT v FROM store_meta WHERE k = 'store_id'")
        .fetch_one(&mut *conn)
        .await?;
    let rows = sqlx::query(
        "SELECT id, at, principal_id, username_tried, kind, remote_addr, user_agent, detail \
         FROM auth_events ORDER BY id",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut head = db_head(conn).await?;
    let n = rows.len();
    for r in &rows {
        let legacy_id: i64 = r.get(0);
        let at: i64 = r.get(1);
        let kind: String = r.get(4);
        let kind = static_kind(&format!("auth.{kind}"))
            .ok_or_else(|| anyhow::anyhow!("auth_events #{legacy_id}: unknown kind {kind}"))?;
        let username_tried: Option<String> = r.get(3);
        let raw: Option<String> = r.get(7);
        let mut detail = match raw.as_deref().map(serde_json::from_str::<Value>) {
            Some(Ok(Value::Object(o))) => o,
            Some(Ok(other)) => Map::from_iter([("legacy_detail".to_string(), other)]),
            Some(Err(_)) => Map::from_iter([(
                "legacy_detail".to_string(),
                Value::String(raw.clone().unwrap_or_default()),
            )]),
            None => Map::new(),
        };
        detail.insert("legacy_id".into(), Value::from(legacy_id));
        if let Some(u) = &username_tried {
            detail.insert("username_tried".into(), Value::from(u.clone()));
        }
        detail.insert("backfilled".into(), Value::Bool(true));
        let mut ev = Event::new(kind, AuditVia::System).detail(Value::Object(detail));
        ev.principal_id = r.get(2);
        ev.remote_addr = r.get(5);
        ev.user_agent = r.get(6);
        head = Some(insert_row_at(conn, &store_id, head, &ev, at.saturating_mul(1000)).await?);
    }
    sqlx::raw_sql("ALTER TABLE auth_events RENAME TO auth_events_legacy")
        .execute(&mut *conn)
        .await?;
    sqlx::raw_sql(LEGACY_TRIGGERS).execute(&mut *conn).await?;
    sqlx::raw_sql(VIEW_SQL).execute(&mut *conn).await?;
    tracing::info!("access store: absorbed {n} auth_events rows into the audit chain (§6.6)");
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::access::audit::verify_boot;
    use crate::access::migrations::{migrate, Seed};
    use crate::access::store::StoreTier;
    use crate::executor::PrincipalId;
    use crate::server::operator::auth_events::{record, AuthEvent, AuthEventKind};
    use crate::server::operator::migrations::{apply, Policy, ACCOUNTS};
    use sqlx::Connection;

    const COLS: [&str; 8] = [
        "id",
        "at",
        "principal_id",
        "username_tried",
        "kind",
        "remote_addr",
        "user_agent",
        "detail",
    ];

    type Legacy = (
        i64,
        i64,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    );

    async fn accounts_db() -> (SqliteConnection, PrincipalId) {
        let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        apply(&mut conn, &ACCOUNTS, Policy::Migrate).await.unwrap();
        let id = PrincipalId::new_v7();
        sqlx::query(
            "INSERT INTO accounts (principal_id, username, unix_name, unix_uid, unix_gid, home, \
             created_at, updated_at) VALUES (?, 'ada', 'ik-ada', 20001, 20001, '/h', 0, 0)",
        )
        .bind(id.to_string())
        .execute(&mut conn)
        .await
        .unwrap();
        (conn, id)
    }

    fn t1() -> Seed {
        Seed {
            tier: StoreTier::T1,
            host_device_name: None,
            host_platform: None,
        }
    }

    /// A-19: after absorption `auth_events` (the view) has §6.1's columns
    /// and the same row set, matched by `detail.legacy_id`; ids are not
    /// preserved (`id` = `seq`); new writes land in the chain.
    #[tokio::test]
    async fn absorption_keeps_the_section_6_1_contract() {
        let (mut conn, ada) = accounts_db().await;
        let mut tx = conn.begin().await.unwrap();
        record(
            &mut tx,
            AuthEvent::new(AuthEventKind::LoginOk)
                .principal(ada)
                .username_tried("ada")
                .remote_addr(Some("10.0.0.2".into()))
                .user_agent(Some("Firefox".into())),
        )
        .await
        .unwrap();
        record(
            &mut tx,
            AuthEvent::new(AuthEventKind::LoginFail)
                .username_tried("mallory")
                .detail(serde_json::json!({ "reason": "unknown_user" })),
        )
        .await
        .unwrap();
        record(
            &mut tx,
            AuthEvent::new(AuthEventKind::SessionsRevoked)
                .principal(ada)
                .detail(serde_json::json!({ "via": "cli", "session_epoch": 1 })),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let legacy: Vec<Legacy> = sqlx::query_as(
            "SELECT id, at, principal_id, username_tried, kind, remote_addr, user_agent, detail \
             FROM auth_events ORDER BY id",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap();
        assert_eq!(legacy.len(), 3);

        migrate(&mut conn, &t1()).await.unwrap();
        assert_eq!(shape(&mut conn).await.unwrap(), AuthEventsShape::View);

        // The same columns, in §6.1's order.
        let cols: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('auth_events')")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(cols, COLS);

        // The same row set, matched by legacy_id.
        let view: Vec<Legacy> = sqlx::query_as(
            "SELECT id, at, principal_id, username_tried, kind, remote_addr, user_agent, detail \
             FROM auth_events ORDER BY id",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap();
        assert_eq!(view.len(), legacy.len());
        for (l, v) in legacy.iter().zip(&view) {
            let d: Value = serde_json::from_str(v.7.as_deref().unwrap()).unwrap();
            assert_eq!(d["legacy_id"], l.0, "matched by legacy_id");
            assert_eq!(d["backfilled"], true);
            assert_eq!(
                (v.1, &v.2, &v.3, &v.4, &v.5, &v.6),
                (l.1, &l.2, &l.3, &l.4, &l.5, &l.6)
            );
            if let Some(orig) = &l.7 {
                let orig: Value = serde_json::from_str(orig).unwrap();
                for (k, val) in orig.as_object().unwrap() {
                    assert_eq!(&d[k], val, "detail key {k} kept");
                }
            }
            // `id` is the chain seq now, not the legacy id.
            let kind: String = sqlx::query_scalar("SELECT kind FROM audit_events WHERE seq = ?")
                .bind(v.0)
                .fetch_one(&mut conn)
                .await
                .unwrap();
            assert_eq!(kind, format!("auth.{}", l.4));
        }
        // seq 1 is store.created; the backfill follows it.
        assert_eq!(view[0].0, 2);

        // The legacy rows are kept, append-only.
        let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM auth_events_legacy")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(kept, 3);
        assert!(sqlx::query("DELETE FROM auth_events_legacy")
            .execute(&mut conn)
            .await
            .unwrap_err()
            .to_string()
            .contains("append-only"));

        // New writes go through the chain (the R-2 re-point).
        let mut tx = conn.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let id = record(
            &mut tx,
            AuthEvent::new(AuthEventKind::LoginThrottled)
                .username_tried("mallory")
                .detail(serde_json::json!({ "retry_after_ms": 30000 })),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let (kind, username, cat): (String, Option<String>, String) = sqlx::query_as(
            "SELECT v.kind, v.username_tried, a.category FROM auth_events v \
             JOIN audit_events a ON a.seq = v.id WHERE v.id = ?",
        )
        .bind(id)
        .fetch_one(&mut conn)
        .await
        .unwrap();
        assert_eq!(
            (kind.as_str(), username.as_deref(), cat.as_str()),
            ("login_throttled", Some("mallory"), "access")
        );
        let still_legacy: i64 = sqlx::query_scalar("SELECT count(*) FROM auth_events_legacy")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(still_legacy, 3);

        let store_id: String = sqlx::query_scalar("SELECT v FROM store_meta WHERE k = 'store_id'")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        let v = verify_boot::verify(&mut conn, &store_id).await.unwrap();
        assert!(v.ok(), "{v:?}");
        assert_eq!(v.rows, 5);
    }

    /// A store migrated before the absorb (access set at 1) absorbs on the
    /// next broker start; the step is a no-op on T0.
    #[tokio::test]
    async fn an_upgrade_absorbs_once_and_t0_is_a_no_op() {
        let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        migrate(
            &mut conn,
            &Seed {
                tier: StoreTier::T0,
                host_device_name: Some("h".into()),
                host_platform: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(shape(&mut conn).await.unwrap(), AuthEventsShape::Absent);
        let applied: Vec<String> =
            sqlx::query_scalar("SELECT name FROM _operator_migrations WHERE \"set\" = 'access'")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(applied, ["0001_core", "0002_absorb_auth_events"]);

        let (mut conn, ada) = accounts_db().await;
        migrate(&mut conn, &t1()).await.unwrap();
        // Absorbing twice is a no-op (the view is already there).
        let mut tx = conn.begin().await.unwrap();
        absorb(&mut tx).await.unwrap();
        record(
            &mut tx,
            AuthEvent::new(AuthEventKind::Logout).principal(ada),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM auth_events WHERE kind = 'logout'")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(n, 1);
    }
}
