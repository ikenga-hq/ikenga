//! OS-wide shortcuts (G-ACTIONS §6, DEC-60; WP-54).
//!
//! The three shipped OS-wide shortcuts are the registry's `os.*` commands
//! (`scope: 'os'` entries in `src/lib/keymap/defaults.ts`). What is
//! registered with the OS is the **effective** default + personal OS rules:
//! the defaults below at boot (so summon works before the webview loads),
//! then whatever the primary window pushes through `os_shortcuts_apply` —
//! on load and again whenever a personal `keybindings.json` rebind changes
//! them. Registration stays tolerant per shortcut: one failure is logged and
//! reported in its status (the Keys tab shows "not registered: <reason>") and
//! never blocks the others. The handler dispatches by the bound command, not
//! by a fixed key, so a rebound key runs the same command.
//!
//! Moved out of `lib.rs` in the WP-19 final slice (part A). `lib.rs` calls
//! the two boot hooks ([`global_shortcut_plugin`],
//! [`register_default_os_shortcuts`]); `commands/registry.rs` registers
//! [`os_shortcuts_apply`].

use tauri::Manager;

/// One effective OS rule: an action id and its key in the registry grammar
/// (`alt+space`, `ctrl+alt+shift+s`).
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct OsShortcutRule {
    command: String,
    key: String,
}

/// Per-rule registration result, returned to the webview.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct OsShortcutStatus {
    command: String,
    key: String,
    registered: bool,
    reason: Option<String>,
}

/// The shortcuts currently registered with the OS and the command each runs.
///
/// `bound` is only ever held for a copy-in / copy-out, never across an OS
/// (un)register call: on Linux/X11 global-hotkey runs the plugin handler on
/// its own thread, and that handler reads `bound` (`command_for`) while a
/// `register` from another thread waits on that same thread — holding
/// `bound` across `register` would deadlock the moment an OS shortcut is
/// pressed during a re-apply. `apply` serializes whole re-applies instead;
/// the handler never takes it.
#[derive(Default)]
struct OsShortcuts {
    bound: std::sync::Mutex<Vec<(tauri_plugin_global_shortcut::Shortcut, String)>>,
    apply: std::sync::Mutex<()>,
}

impl OsShortcuts {
    fn command_for(&self, shortcut: &tauri_plugin_global_shortcut::Shortcut) -> Option<String> {
        let bound = self.bound.lock().unwrap_or_else(|p| p.into_inner());
        bound
            .iter()
            .find(|(bound_shortcut, _)| bound_shortcut == shortcut)
            .map(|(_, command)| command.clone())
    }
}

/// The default-layer OS rules — mirrors `DEFAULT_KEYMAP`'s `scope: 'os'`
/// entries. `os.summon` on Windows/Linux stays Super+Space as shipped; on
/// Windows it may collide with the input-language switcher (open question,
/// `04` Round 37 — flagged, not changed here).
fn default_os_rules() -> Vec<OsShortcutRule> {
    let summon = if cfg!(target_os = "macos") {
        "alt+space"
    } else {
        "meta+space"
    };
    [
        ("os.summon", summon),
        ("os.screenshot-window", "ctrl+alt+shift+s"),
        ("os.screenshot-pane", "ctrl+alt+shift+p"),
    ]
    .into_iter()
    .map(|(command, key)| OsShortcutRule {
        command: command.to_string(),
        key: key.to_string(),
    })
    .collect()
}

/// A registry key name (G-ACTIONS §3.1) → the physical key the OS registers.
/// `plus` and `?` are character keys with no fixed position, so they cannot
/// be OS-wide.
fn os_key_code(name: &str) -> Option<tauri_plugin_global_shortcut::Code> {
    use tauri_plugin_global_shortcut::Code;
    let code = match name {
        "a" => Code::KeyA,
        "b" => Code::KeyB,
        "c" => Code::KeyC,
        "d" => Code::KeyD,
        "e" => Code::KeyE,
        "f" => Code::KeyF,
        "g" => Code::KeyG,
        "h" => Code::KeyH,
        "i" => Code::KeyI,
        "j" => Code::KeyJ,
        "k" => Code::KeyK,
        "l" => Code::KeyL,
        "m" => Code::KeyM,
        "n" => Code::KeyN,
        "o" => Code::KeyO,
        "p" => Code::KeyP,
        "q" => Code::KeyQ,
        "r" => Code::KeyR,
        "s" => Code::KeyS,
        "t" => Code::KeyT,
        "u" => Code::KeyU,
        "v" => Code::KeyV,
        "w" => Code::KeyW,
        "x" => Code::KeyX,
        "y" => Code::KeyY,
        "z" => Code::KeyZ,
        "0" => Code::Digit0,
        "1" => Code::Digit1,
        "2" => Code::Digit2,
        "3" => Code::Digit3,
        "4" => Code::Digit4,
        "5" => Code::Digit5,
        "6" => Code::Digit6,
        "7" => Code::Digit7,
        "8" => Code::Digit8,
        "9" => Code::Digit9,
        "`" => Code::Backquote,
        "-" => Code::Minus,
        "=" => Code::Equal,
        "[" => Code::BracketLeft,
        "]" => Code::BracketRight,
        "\\" => Code::Backslash,
        ";" => Code::Semicolon,
        "'" => Code::Quote,
        "," => Code::Comma,
        "." => Code::Period,
        "/" => Code::Slash,
        "space" => Code::Space,
        "enter" => Code::Enter,
        "escape" => Code::Escape,
        "tab" => Code::Tab,
        "backspace" => Code::Backspace,
        "delete" => Code::Delete,
        "insert" => Code::Insert,
        "home" => Code::Home,
        "end" => Code::End,
        "pageup" => Code::PageUp,
        "pagedown" => Code::PageDown,
        "arrowup" => Code::ArrowUp,
        "arrowdown" => Code::ArrowDown,
        "arrowleft" => Code::ArrowLeft,
        "arrowright" => Code::ArrowRight,
        "f1" => Code::F1,
        "f2" => Code::F2,
        "f3" => Code::F3,
        "f4" => Code::F4,
        "f5" => Code::F5,
        "f6" => Code::F6,
        "f7" => Code::F7,
        "f8" => Code::F8,
        "f9" => Code::F9,
        "f10" => Code::F10,
        "f11" => Code::F11,
        "f12" => Code::F12,
        "f13" => Code::F13,
        "f14" => Code::F14,
        "f15" => Code::F15,
        "f16" => Code::F16,
        "f17" => Code::F17,
        "f18" => Code::F18,
        "f19" => Code::F19,
        "f20" => Code::F20,
        "f21" => Code::F21,
        "f22" => Code::F22,
        "f23" => Code::F23,
        "f24" => Code::F24,
        _ => return None,
    };
    Some(code)
}

/// Parse one stroke of the registry grammar into an OS shortcut. `mod` is ⌘
/// on macOS and Ctrl elsewhere; `meta` is the literal ⌘ / Win / Super key.
fn parse_os_key(key: &str) -> Result<tauri_plugin_global_shortcut::Shortcut, String> {
    use tauri_plugin_global_shortcut::{Modifiers, Shortcut};

    let key = key.trim();
    if key.is_empty() {
        return Err("empty key".to_string());
    }
    if key.contains(' ') {
        return Err("a chord cannot be an OS-wide shortcut".to_string());
    }
    let parts: Vec<&str> = key.split('+').collect();
    let Some((name, modifier_names)) = parts.split_last() else {
        return Err("empty key".to_string());
    };
    let mut modifiers = Modifiers::empty();
    for m in modifier_names {
        let flag = match *m {
            "mod" => {
                if cfg!(target_os = "macos") {
                    Modifiers::SUPER
                } else {
                    Modifiers::CONTROL
                }
            }
            "ctrl" => Modifiers::CONTROL,
            "meta" => Modifiers::SUPER,
            "alt" => Modifiers::ALT,
            "shift" => Modifiers::SHIFT,
            other => return Err(format!("unknown modifier `{other}`")),
        };
        modifiers |= flag;
    }
    let code = os_key_code(name).ok_or_else(|| format!("`{name}` cannot be an OS-wide key"))?;
    // A bare key would be taken from every app on the machine. Only the
    // function keys may go without a modifier.
    let function_key =
        name.len() > 1 && name.starts_with('f') && name[1..].chars().all(|c| c.is_ascii_digit());
    if modifiers.is_empty() && !function_key {
        return Err(format!(
            "`{name}` needs a modifier to be OS-wide (it would take the key from every app)"
        ));
    }
    let modifiers = if modifiers.is_empty() {
        None
    } else {
        Some(modifiers)
    };
    Ok(Shortcut::new(modifiers, code))
}

/// A command an OS rule may not name (§6): a package action (DEC-54) or a
/// hosted command, whose owner has no focus while Ikenga is unfocused.
fn os_command_refusal(command: &str) -> Option<&'static str> {
    if command.contains(':') {
        return Some("a package action cannot be OS-wide");
    }
    if command.starts_with("terminal.")
        || matches!(
            command,
            "companion.send" | "companion.new-run" | "companion.persistent-run"
        )
    {
        return Some("a hosted command cannot be OS-wide");
    }
    None
}

/// Replace every registered OS shortcut with `rules`. Tolerant per rule.
fn apply_os_shortcuts(app: &tauri::AppHandle, rules: &[OsShortcutRule]) -> Vec<OsShortcutStatus> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    let status = |rule: &OsShortcutRule, reason: Option<String>| OsShortcutStatus {
        command: rule.command.clone(),
        key: rule.key.clone(),
        registered: reason.is_none(),
        reason,
    };
    let Some(state) = app.try_state::<OsShortcuts>() else {
        return rules
            .iter()
            .map(|rule| status(rule, Some("OS shortcuts are not initialised".to_string())))
            .collect();
    };
    let _applying = state.apply.lock().unwrap_or_else(|p| p.into_inner());
    // Take the old set and release `bound` before touching the OS (see
    // `OsShortcuts`): the handler may need it while we (un)register.
    let old = std::mem::take(&mut *state.bound.lock().unwrap_or_else(|p| p.into_inner()));
    for (shortcut, command) in old {
        if let Err(e) = app.global_shortcut().unregister(shortcut) {
            // `log::` macros are dropped in this crate (no log→tracing
            // bridge); use tracing so the warning actually emits.
            tracing::warn!("OS shortcut for {command} not unregistered (continuing): {e}");
        }
    }

    let mut bound: Vec<(tauri_plugin_global_shortcut::Shortcut, String)> = Vec::new();
    let mut out = Vec::with_capacity(rules.len());
    for rule in rules {
        if let Some(reason) = os_command_refusal(&rule.command) {
            out.push(status(rule, Some(reason.to_string())));
            continue;
        }
        let shortcut = match parse_os_key(&rule.key) {
            Ok(shortcut) => shortcut,
            Err(reason) => {
                out.push(status(rule, Some(reason)));
                continue;
            }
        };
        if let Some((_, other)) = bound.iter().find(|(b, _)| *b == shortcut) {
            let reason = format!("the key is already OS-wide for {other}");
            out.push(status(rule, Some(reason)));
            continue;
        }
        match app.global_shortcut().register(shortcut) {
            Ok(()) => {
                bound.push((shortcut, rule.command.clone()));
                out.push(status(rule, None));
            }
            Err(e) => {
                tracing::warn!(
                    "OS shortcut {} → {} not registered (continuing): {e}",
                    rule.key,
                    rule.command
                );
                out.push(status(rule, Some(e.to_string())));
            }
        }
    }
    *state.bound.lock().unwrap_or_else(|p| p.into_inner()) = bound;
    out
}

/// Registers the effective default + personal OS rules the primary window
/// computed (`startOsShortcutSync`, `src/lib/keymap/dispatcher.ts`),
/// replacing the previous set. Sync on purpose: it runs on the main thread,
/// where the OS registration has to happen.
#[tauri::command]
pub(crate) fn os_shortcuts_apply(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    rules: Vec<OsShortcutRule>,
) -> Vec<OsShortcutStatus> {
    // Only the primary window computes the effective default + personal OS
    // rules (DEC-60). Detached windows share `allow-app-commands`, so refuse
    // them here rather than trust the capability set.
    if window.label() != "main" {
        return rules
            .into_iter()
            .map(|rule| OsShortcutStatus {
                command: rule.command,
                key: rule.key,
                registered: false,
                reason: Some("only the main window applies OS shortcuts".to_string()),
            })
            .collect();
    }
    apply_os_shortcuts(&app, &rules)
}

/// Run the command an OS shortcut is bound to.
fn run_os_command(app: &tauri::AppHandle, command: &str) {
    match command {
        "os.summon" => {
            // Summon always targets the PRIMARY window — "bring Ikenga to
            // the front" means the main window, not whatever is focused
            // (multi-window: intentionally stays "main").
            if let Some(window) = app.get_webview_window("main") {
                let visible = window.is_visible().unwrap_or(false);
                if visible && window.is_focused().unwrap_or(false) {
                    let _ = window.hide();
                } else {
                    let _ = window.unminimize();
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        }
        "os.screenshot-window" | "os.screenshot-pane" => {
            // Target the focused window that actually hosts the screenshot
            // listener (`useScreenshotListener`, mounted only inside
            // `<Workspace/>`). `focused_listener_window_label` returns a
            // focused `Workspace`-kind spawned window (Flavor B) if any,
            // else `None` — deliberately never a `single-surface`/`pane-set`
            // detached window or a pkg-pane child webview, which have no
            // listener. `None` → "main", exactly today's behavior; on
            // WebKitGTK `is_focused` can under-report, which also falls back
            // to "main" (safe). Detached-window capture also needs the
            // listener + `capture_window_png` de-"main"'d before it lights
            // up in practice.
            let kind = if command == "os.screenshot-window" {
                "window"
            } else {
                "pane-focused"
            };
            let target = crate::window::focused_listener_window_label(app)
                .unwrap_or_else(|| "main".to_string());
            let _ = crate::window::emit_to_label(
                app,
                &target,
                "screenshot://shortcut",
                serde_json::json!({ "kind": kind }),
            );
        }
        // Any other action a personal OS rule names (§6): the primary
        // window's dispatcher runs it through the command table.
        other => {
            let _ = crate::window::emit_to_label(
                app,
                "main",
                "keymap://os-command",
                serde_json::json!({ "command": other }),
            );
        }
    }
}

/// Build the global-shortcut plugin. The handler looks the pressed shortcut
/// up in `OsShortcuts` and runs the command it is bound to; the screenshot
/// commands emit `screenshot://shortcut` events that the FE picks up and
/// routes back through `screenshot_window` / `screenshot_pane`. Doing the
/// focused-pane resolution in the FE avoids mirroring `usePaneStore` on the
/// Rust side just for one handler.
pub(crate) fn global_shortcut_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    use tauri_plugin_global_shortcut::{Builder, ShortcutState};

    Builder::new()
        .with_handler(move |app, shortcut, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            let Some(command) = app
                .try_state::<OsShortcuts>()
                .and_then(|state| state.command_for(shortcut))
            else {
                return;
            };
            run_os_command(app, &command);
        })
        .build()
}

/// Boot: register the default OS rules until the webview pushes the
/// effective set. Tolerant — a clash on one never kills the others.
pub(crate) fn register_default_os_shortcuts(app: &tauri::AppHandle) {
    app.manage(OsShortcuts::default());
    for status in apply_os_shortcuts(app, &default_os_rules()) {
        if !status.registered {
            tracing::warn!(
                "OS shortcut {} → {} not registered (continuing): {}",
                status.key,
                status.command,
                status.reason.unwrap_or_default()
            );
        }
    }
}
