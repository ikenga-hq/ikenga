//! Desktop facade over the settings substrate.
//!
//! The implementation lives in [`crate::server::shared::settings`], which
//! compiles into both binaries so the daemon's `settings_*` RPC arms run the
//! same code (WP-19). This module keeps every `crate::settings::…` path the
//! desktop already uses, and adds the one desktop-only piece: the
//! `AppHandle`-backed constructor, whose notifier emits `settings://changed`
//! exactly as the manager always did.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::{AppHandle, Emitter};

use crate::commands::db::PaDb;

pub use crate::server::shared::settings::{
    migrate, schema, scope, ChangeNotifier, SettingsChangeEvent, SettingsManager,
    SettingsReadResult, SettingsScope,
};
// Used by the WP-37 migration rehearsal (`rehearsal_5a.rs`, test-only).
#[cfg(test)]
pub(crate) use crate::server::shared::settings::{effective_document, project_roots_from_pool};

impl SettingsManager {
    /// The desktop constructor (called from `lib.rs` setup). Home is the
    /// signed-in user's (`platform::home_dir`, `.` if none is set — unchanged),
    /// and every change is emitted to the webview as `settings://changed`.
    pub fn new(app: AppHandle, db: Arc<PaDb>, app_data_dir: PathBuf) -> Self {
        let home = crate::platform::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let notifier: ChangeNotifier = Arc::new(move |path: &Path| {
            let _ = app.emit(
                "settings://changed",
                SettingsChangeEvent {
                    path: path.to_string_lossy().into_owned(),
                },
            );
        });
        Self::with_notifier(Some(notifier), db, app_data_dir, home)
    }
}
