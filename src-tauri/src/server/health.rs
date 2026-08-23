use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;

static START_TIME_SECS: AtomicU64 = AtomicU64::new(0);

pub fn init_uptime() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    START_TIME_SECS.store(now, Ordering::Relaxed);
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub name: &'static str,
    pub version: &'static str,
    pub status: &'static str,
    pub uptime_secs: u64,
}

pub async fn health_handler() -> impl IntoResponse {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let start = START_TIME_SECS.load(Ordering::Relaxed);
    let uptime = if start > 0 { now.saturating_sub(start) } else { 0 };

    Json(HealthResponse {
        ok: true,
        name: "ikenga-server",
        version: env!("CARGO_PKG_VERSION"),
        status: "ready",
        uptime_secs: uptime,
    })
}
