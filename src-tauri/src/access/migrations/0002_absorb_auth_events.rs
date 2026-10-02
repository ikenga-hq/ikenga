//! `access/0002_absorb_auth_events` (G-ACCESS §6.6, §8.3; WP-77, W5): a Rust
//! step, run by [`super::migrate`] inside the batch's one `BEGIN
//! IMMEDIATE`. T1 only; on T0 — where `auth_events` doesn't exist — it
//! records itself as applied and does nothing. The work is
//! [`crate::access::audit::absorb::absorb`].

use sqlx::SqliteConnection;

use crate::access::store::StoreTier;

pub(super) async fn run(conn: &mut SqliteConnection, tier: StoreTier) -> anyhow::Result<()> {
    match tier {
        StoreTier::T0 => Ok(()),
        StoreTier::T1 => crate::access::audit::absorb::absorb(conn).await,
    }
}
