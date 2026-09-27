//! Desktop facade over the actions / keybindings file layer (WP-50).
//!
//! The implementation lives in [`crate::server::shared::actions`], which
//! compiles into both binaries so the daemon's `actions_*` /
//! `keybindings_write` RPC arms run the same code (WP-19 slice 5a). This
//! module keeps every `crate::actions::…` path the desktop already uses, and
//! adds the desktop-only pieces: the `AppHandle`-backed constructor, whose
//! notifier emits `actions://changed` exactly as the manager always did, and
//! the OS opener behind `actions_open_file` (which spawns, so the daemon never
//! serves it).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::{AppHandle, Emitter};

use crate::commands::db::PaDb;
use crate::settings::SettingsScope;

pub use crate::server::shared::actions::{
    schema, scope, trust, watch, ActionsFilesResult, ActionsManager, ActionsWriteResult,
    ScopeFiles, TrustGrantRequest, TrustRevokeRequest, TrustStatus,
};

use self::schema::FileKind;
use self::watch::{ActionsChangeEvent, ActionsNotifier, CHANGED_EVENT};

impl ActionsManager {
    /// The desktop constructor (called from `lib.rs` setup). Home is the
    /// signed-in user's (`platform::home_dir`, `.` if none is set — unchanged),
    /// the trust record lives in `app_data_dir`, and every change is emitted to
    /// the webview as `actions://changed`.
    pub fn new(app: AppHandle, db: Arc<PaDb>, app_data_dir: PathBuf) -> Self {
        let home = crate::platform::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let notifier: ActionsNotifier = Arc::new(move |event: ActionsChangeEvent| {
            let _ = app.emit(CHANGED_EVENT, event);
        });
        Self::with_notifier(Some(notifier), db, &app_data_dir, home)
    }

    /// Creates the file with an empty valid skeleton when absent, then opens
    /// it with the OS handler. Returns the path. See
    /// [`ActionsManager::open_file_with`] for the link refusal.
    pub async fn open_file(
        &self,
        kind: FileKind,
        scope: SettingsScope,
        project_id: Option<&str>,
    ) -> Result<String, String> {
        self.open_file_with(kind, scope, project_id, open_path)
            .await
    }
}

fn open_path(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("explorer.exe");
        command.arg(path);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(path);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(path);
        command
    };
    command
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("open {}: {e}", path.display()))
}
