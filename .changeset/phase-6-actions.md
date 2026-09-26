---
"ikenga-desktop": minor
---

**Phase 6: actions, menus and keys.** Every menu item, key and shortcut in the shell is now data that you can see, rebind, reorder and extend. It lives in two files at two scopes: `~/.ikenga/{actions,keybindings}.json` for personal, and `<project>/.ikenga/…` for project.

- **One key dispatcher.** The registry is the single place keys fire from. It covers the workspace, the rail, the palette, zoom, the terminal chord table and the leftover widget handlers.
  - Chords wait up to 900 ms, and only when a chord starting with ⌘K is actually bound; otherwise ⌘K opens the palette immediately.
  - Typing and IME never fire a shortcut.
  - The macOS menu-bar accelerators are deduplicated, so a key fires once.
  - **Windows/Linux:** Alt+1–6 now focuses pane N. Ctrl+1–3 still switches the rail.
- **OS-wide shortcuts.** Summon, window screenshot and pane screenshot are ordinary keymap rows with `scope: 'os'`. You can rebind them in your personal file. A project or package can never claim an OS-wide key.
- **Menus render from data.** The Explorer, tab, pane `⋯`, status bar, rail, palette groups and the native menu (76 items across 8 files) all come from the effective model.
  - Reorder or hide an item in `actions.json` and it applies live, with no restart. Locked items can move but never hide.
  - Package actions appear where their manifest places them.
- **Your own actions.** There are six run kinds: `chi`, `shell`, `iyke`, `skill`, `workflow` and `open`, plus built-in and package kinds.
  - **Trust in a project.** In an untrusted project, `shell`, `iyke`, `skill` and `workflow` actions refuse until you trust them in the trust sheet, and editing the command asks again. An untrusted project's keybindings are "held until trusted".
  - **Test run** previews rather than acts. For `shell`, and for `iyke` actions that send a POST, it shows what would run instead of running it.
- **Settings → Actions, menus and keys (D-06).** It has five parts: the Actions list, Editor, Menus, Keys (with conflicts), and Import. Import takes a package, the core of your VS Code `keybindings.json`, or a teammate's project file. An import never overrides a binding you already have.
- **`iyke`.** The bridge is now at API level 4, with actions, menus and keys routes. The matching commands (`iyke actions`, `iyke menus` and `iyke keys`) ship in iyke-cli v0.5.0.
- **Packages.** A manifest can request a single-stroke `key` on `ui.context_actions[]` (G-PKG-KEY). It is granted only if the key is free. `ui.command_palette[].action` is now typed.
- CI now builds against `@ikenga/contract` HEAD again. The pin added during Phase 6 has been removed.
