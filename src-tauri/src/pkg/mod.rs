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
#[cfg(feature = "desktop")]
pub mod mcp_runtime;
#[cfg(feature = "desktop")]
pub mod npm_install;
#[cfg(feature = "desktop")]
pub mod permissions_check;
#[cfg(feature = "desktop")]
pub mod registries;
pub mod registry;
#[cfg(feature = "desktop")]
pub mod signature;
#[cfg(feature = "desktop")]
pub mod skill_actions;
#[cfg(feature = "desktop")]
pub mod source;
#[cfg(feature = "desktop")]
pub mod trust;
#[cfg(feature = "desktop")]
pub mod webview;

#[cfg(feature = "desktop")]
pub use engine_adapter::EngineAdaptersRegistry;
#[cfg(feature = "desktop")]
pub use kernel::{DiscoveredPkg, InstalledSummary, Kernel, KernelStatus, PkgHealthIssue};
#[cfg(feature = "desktop")]
pub use lifecycle::SidecarSupervisor;
#[cfg(feature = "desktop")]
pub use npm_install::materialize_npm_deps;
pub use registry::Registry;
#[cfg(feature = "desktop")]
pub use source::InstallSource;
