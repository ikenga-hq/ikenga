//! Concrete `Registry` implementations. Add one module per registry; expose
//! the `*Registry` struct and (optionally) lookup APIs other code needs.

pub mod activity_bar;
pub mod companion_panels;
pub mod context_actions;
pub mod cron;
pub mod engine_assets;
pub mod explorer_sections;
pub mod iyke_routes;
pub mod mcp;
pub mod permissions;
pub mod queries;
pub mod settings;
pub mod sidecars;
pub mod ui_routes;
pub mod views;
pub mod widgets;

pub use activity_bar::{ActivityBarBadge, ActivityBarRegistry};
pub use companion_panels::CompanionPanelsRegistry;
pub use context_actions::ContextActionsRegistry;
pub use cron::CronRegistry;
pub use engine_assets::EngineAssetsRegistry;
pub use explorer_sections::ExplorerSectionsRegistry;
pub use iyke_routes::IykeRoutesRegistry;
pub use mcp::McpRegistry;
pub use permissions::PermissionsRegistry;
pub use queries::QueriesRegistry;
pub use settings::SettingsRegistry;
pub use sidecars::SidecarsRegistry;
pub use ui_routes::UiRoutesRegistry;
pub use views::ViewsRegistry;
pub use widgets::WidgetsRegistry;
