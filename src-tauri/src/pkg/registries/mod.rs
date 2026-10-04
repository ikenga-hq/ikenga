//! Concrete `Registry` implementations. Add one module per registry; expose
//! the `*Registry` struct and (optionally) lookup APIs other code needs.
//!
//! This module compiles into both binaries, but only `ui_routes` does: the
//! headless daemon registers discovered pkgs' UI routes so its
//! `pkg_kernel_status` can report `registries.ui_routes` (the FE pkg route
//! resolver reads it). Every other registry is individually
//! `#[cfg(feature = "desktop")]` — gate any new one the same way unless the
//! daemon actually runs it.

pub mod activity_bar;
#[cfg(feature = "desktop")]
pub mod companion_panels;
#[cfg(feature = "desktop")]
pub mod context_actions;
#[cfg(feature = "desktop")]
pub mod cron;
#[cfg(feature = "desktop")]
pub mod engine_assets;
#[cfg(feature = "desktop")]
pub mod explorer_sections;
#[cfg(feature = "desktop")]
pub mod iyke_routes;
#[cfg(feature = "desktop")]
pub mod mcp;
#[cfg(feature = "desktop")]
pub mod permissions;
#[cfg(feature = "desktop")]
pub mod queries;
#[cfg(feature = "desktop")]
pub mod settings;
#[cfg(feature = "desktop")]
pub mod sidecars;
pub mod ui_routes;
pub mod views;
#[cfg(feature = "desktop")]
pub mod widgets;

pub use activity_bar::{ActivityBarBadge, ActivityBarRegistry};
#[cfg(feature = "desktop")]
pub use companion_panels::CompanionPanelsRegistry;
#[cfg(feature = "desktop")]
pub use context_actions::ContextActionsRegistry;
#[cfg(feature = "desktop")]
pub use cron::CronRegistry;
#[cfg(feature = "desktop")]
pub use engine_assets::EngineAssetsRegistry;
#[cfg(feature = "desktop")]
pub use explorer_sections::ExplorerSectionsRegistry;
#[cfg(feature = "desktop")]
pub use iyke_routes::IykeRoutesRegistry;
#[cfg(feature = "desktop")]
pub use mcp::McpRegistry;
#[cfg(feature = "desktop")]
pub use permissions::PermissionsRegistry;
#[cfg(feature = "desktop")]
pub use queries::QueriesRegistry;
#[cfg(feature = "desktop")]
pub use settings::SettingsRegistry;
#[cfg(feature = "desktop")]
pub use sidecars::SidecarsRegistry;
pub use ui_routes::UiRoutesRegistry;
pub use views::ViewsRegistry;
#[cfg(feature = "desktop")]
pub use widgets::WidgetsRegistry;
