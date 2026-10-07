//! Composable-app package kernel.
//!
//! See `kernel.rs` for the lifecycle entry points and `manifest.rs` for the
//! on-disk contract. Concrete registries live in `registries/`.
//!
//! Wiring: built in `lib.rs::run()::setup`, stored in app state, exposed via
//! the `pkg_*` Tauri commands in `commands::pkg`.
//!
//! # Two builds
//!
//! `manifest` and `registry` compile into BOTH binaries: they are pure serde
//! + a trait definition and have never referenced tauri. The headless daemon
//! reads manifests to serve installed pkg bundles read-only
//! (`server::pkg_static`).
//!
//! So do the read-side pieces the daemon's `pkg_kernel_status` /
//! `list_skill_actions` arms need (WP-19): `status` (the wire shape and
//! `assemble_status`, shared with `Kernel::status`), `source` (pure serde),
//! `skill_actions` (std + serde_yaml fs reads), `settings_values` (the
//! `pkg_settings` table + schema-default merge), and — from `registries` —
//! ONLY `ui_routes`. Every other registry stays desktop-gated inside
//! `registries/mod.rs`.
//!
//! Everything else here is desktop-only, and not because of a missing gate —
//! the kernel holds a non-optional `AppHandle`, `webview.rs` drives real
//! `tauri::Webview` windows, and lifecycle spawns supervised sidecars. The
//! daemon deliberately has no install / trust / lifecycle machinery at all.

#[cfg(feature = "desktop")]
pub mod cap_snapshot;
/// Host-side database sandbox for pkg backend processes (WP-23 / D-18).
#[cfg(feature = "desktop")]
pub mod db_scope;
// WP-02 foundation: detection + launcher are standalone until the kernel/command
// wiring lands in later WPs (WP-04 lifecycle, WP-07 routing). Allow dead-code so
// the unconsumed public API doesn't warn in the interim.
#[cfg(feature = "desktop")]
#[allow(dead_code)]
pub mod engine_adapter;
#[cfg(feature = "desktop")]
pub mod engine_adapters;
#[cfg(feature = "desktop")]
pub mod file_watcher;
#[cfg(feature = "desktop")]
pub mod http_proxy;
#[cfg(feature = "desktop")]
pub mod keep_awake;
#[cfg(feature = "desktop")]
pub mod kernel;
#[cfg(feature = "desktop")]
pub mod lifecycle;
pub mod manifest;
// Registers against the v5 contribution registries, which are desktop-only.
#[cfg(all(test, feature = "desktop"))]
mod manifest_v5_parity;
#[cfg(feature = "desktop")]
pub mod mcp_runtime;
#[cfg(feature = "desktop")]
pub mod npm_install;
/// Structured install progress events and cancellation for registry installs.
#[cfg(feature = "desktop")]
pub mod install_progress;
/// Kernel-side `pin_on_install`: rail pins written with a fresh install.
#[cfg(feature = "desktop")]
pub mod pin_on_install;
#[cfg(feature = "desktop")]
pub mod permissions_check;
pub mod registries;
pub mod registry;
// `pkg_settings` table read/upsert + schema-default merge, shared by the
// desktop `pkg_settings_*` commands and the daemon's RPC arms (WP-19).
pub mod settings_values;
#[cfg(feature = "desktop")]
pub mod signature;
pub mod skill_actions;
pub mod source;
pub mod status;
#[cfg(feature = "desktop")]
pub mod trust;
// The `TrustState` wire shape + the sensitive-perms summary, ungated for the
// shared Ngwa snapshot join; `trust` re-exports them.
pub mod trust_state;
#[cfg(feature = "desktop")]
pub(crate) mod uninstall_dir;
#[cfg(feature = "desktop")]
pub mod webview;

#[cfg(feature = "desktop")]
pub use engine_adapter::EngineAdaptersRegistry;
#[cfg(feature = "desktop")]
pub(crate) use kernel::normalize_scope;
#[cfg(feature = "desktop")]
pub use kernel::{DiscoveredPkg, Kernel, PkgHealthIssue, PurgeAllReport, PurgeOutcome};
#[cfg(feature = "desktop")]
pub use lifecycle::SidecarSupervisor;
#[cfg(feature = "desktop")]
pub use npm_install::materialize_npm_deps;
pub use registry::Registry;
pub use source::InstallSource;
pub use status::{assemble_status, InstalledSummary, KernelStatus};
