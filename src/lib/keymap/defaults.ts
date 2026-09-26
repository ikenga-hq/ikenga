// Default bindings — the `default` layer of the registry (G-ACTIONS §2.1).
// The package / personal / project layers (manifest key requests,
// `~/.ikenga/keybindings.json`, `<project>/.ikenga/keybindings.json`) are
// merged on top of this by the effective model (WP-52); nothing here reads
// them.
//
// Every entry is a binding that exists in the shipped app today (`source:
// 'default'`), ported from the `keydown` handlers this registry replaces as
// the label source (`workspace.tsx`, `activity-bar.tsx`) plus
// `native-menu.ts`'s accelerators. Commands whose *handler* still lives in
// its own listener (`workspace.tsx`, `terminal/keybindings.ts` — both WP-54's
// to migrate) are registered here so `labelFor()` and `conflicts()` have one
// source of truth for their key hints, even though nothing here fires them.
// `palette.open` has its own listener (`command-palette.tsx`) because closing
// the palette must bypass the `!inputFocus` guard that opening it observes —
// the palette's own search input is itself a typing target, and ⌘K has
// always closed it from there (G-ACTIONS §4.6: WP-54 adds `palette.close`).
//
// `when` values are DEC-62 expressions (`when.ts`). The pre-v2 words map
// one-to-one (§4.4): `global` → `always`, `not-input` → `!inputFocus`,
// `terminal-focus` → `terminalFocus`. WP-54 applies the DEC-64 re-keys, the
// `dispatchFocus` Companion keys, the palette close keys and the three
// `scope: 'os'` entries; this file only carries the language migration.

import type { WhenClause } from './when';

/** The layer a rule comes from (G-ACTIONS §2.1), lowest first: `default` <
 *  `package` < `personal` (`~/.ikenga/keybindings.json`) < `project`
 *  (`<project>/.ikenga/keybindings.json`). WP-52 renamed the pre-12c `user`
 *  member to `personal` so the union reads like the layer names. */
export type KeymapSource = 'default' | 'package' | 'personal' | 'project';

/** Where a file rule came from: the scope's `keybindings.json` and its
 *  index in `bindings` — what the Keys tab resets / edits (WP-52). */
export interface KeymapRuleOrigin {
	scope: 'personal' | 'project';
	index: number;
}

/** `app` — dispatched in the webview; `os` — registered with
 *  `tauri-plugin-global-shortcut` and fires with Ikenga unfocused (DEC-60,
 *  §6). OS rules are their own conflict space (§5). */
export type KeymapScope = 'app' | 'os';

export interface KeymapEntry {
	/** Stable command id, e.g. `rail.project` (the action id, §10). */
	command: string;
	/** Key sequence — see `platform.ts` for the grammar. One stroke, or a
	 *  two-stroke chord (`mod+k mod+r`). */
	key: string;
	/** DEC-62 expression (`when.ts`). `always` fires even in text inputs;
	 *  `!inputFocus` is the "no focus condition" default. */
	when: WhenClause;
	source: KeymapSource;
	/** Default `'app'`. `'os'` only in the default and personal layers. */
	scope?: KeymapScope;
	/** Human label for the Shortcuts view / `?` overlay. */
	label: string;
	/** Restricts a binding to one platform family (the `platform` field of
	 *  `keybindings.json`, §1.5) — the native menu (and a handful of mac-only
	 *  combos that collide with a terminal binding once `mod` resolves to the
	 *  literal Ctrl key) only apply on macOS; `'other'` is the mirror case
	 *  (`terminal.clear`'s non-mac chord, which is a different combo, not the
	 *  same one platform-gated). */
	platformOnly?: 'mac' | 'other';
	/** Set on `personal` / `project` rules merged from a file (WP-52). */
	origin?: KeymapRuleOrigin;
	/** Set on a `package` rule: the pkg whose key request was granted
	 *  (G-ACTIONS §7.4). */
	pkgId?: string;
	/**
	 * @deprecated Documentation only — `conflicts()` no longer reads it.
	 * Under DEC-59 a same-key pair with different normalized `when`s is
	 * *precedence*, reported separately, never a clash, so no suppression
	 * table is needed. It still records the shipped parallel-fires (both
	 * listeners run today) that DEC-64 re-keys in WP-54, which deletes it.
	 */
	knownOverlap?: string[];
}

export const DEFAULT_KEYMAP: KeymapEntry[] = [
	// --- Rail (src/shell/activity-bar.tsx) — WP-03: Project · Chi · Ngwa ·
	// Settings (spec §2). ⌘4–⌘6 are retired and deliberately left unbound this
	// release (§2 "Retired", §7 Q2) — no entry, so nothing fires and nothing
	// can be labelled with them.
	// `knownOverlap` on rail.project..rail.ngwa: on non-mac, `mod+N` resolves
	// to the literal `ctrl+N` that `pane.focus-N` (`always`, workspace.tsx) is
	// also bound to. Both are live `window` keydown listeners and neither
	// calls `stopPropagation`, so a non-mac Ctrl+1 both switches the rail AND
	// focuses pane 1 today. Under DEC-59 the pair is precedence (different
	// `when`); DEC-64 re-keys pane focus to Alt+1–6 in WP-54.
	{
		command: 'rail.project',
		key: 'mod+1',
		when: '!inputFocus',
		source: 'default',
		label: 'Rail → Project',
		knownOverlap: ['pane.focus-1'],
	},
	{
		command: 'rail.chi',
		key: 'mod+2',
		when: '!inputFocus',
		source: 'default',
		label: 'Rail → Chi',
		knownOverlap: ['pane.focus-2'],
	},
	{
		command: 'rail.ngwa',
		key: 'mod+3',
		when: '!inputFocus',
		source: 'default',
		label: 'Rail → Ngwa',
		knownOverlap: ['pane.focus-3'],
	},
	{
		command: 'rail.settings',
		key: 'mod+,',
		when: '!inputFocus',
		source: 'default',
		label: 'Rail → Settings',
	},

	// --- Command palette (src/shell/command-palette.tsx) ---
	{
		command: 'palette.open',
		key: 'mod+k',
		when: '!inputFocus',
		source: 'default',
		label: 'Command palette',
	},
	// Handler lives in workspace.tsx (WP-20, do-not-touch); registered here so
	// activity-bar.tsx's project-switcher tooltip stops hard-coding "⌘P".
	{
		command: 'palette.projects',
		key: 'mod+p',
		when: '!inputFocus',
		source: 'default',
		label: 'Project switcher',
	},
	// --- Ngwa create (WP-26) ---
	{
		command: 'ngwa.create',
		key: 'mod+n',
		when: '!inputFocus',
		source: 'default',
		label: 'Ngwa → Create',
		knownOverlap: ['menu.new-session'],
	},
	// Documented for `conflicts()` only — same key as `palette.open`, but a
	// different `when` is precedence, not a clash (§6A.5). The terminal's own
	// listener (src/terminal/keybindings.ts, do-not-touch) still owns this.
	// Two entries because the terminal's real default differs by platform
	// (DEFAULT_MAC_KEYBINDINGS vs DEFAULT_LINUX_WIN_KEYBINDINGS.clear there):
	// `Cmd+K` on mac, `Ctrl+Shift+K` elsewhere — not the same combo re-mapped
	// through `mod`, so this needs `platformOnly`, not `resolveCombo`.
	{
		command: 'terminal.clear',
		key: 'mod+k',
		when: 'terminalFocus',
		source: 'default',
		label: 'Clear terminal',
		platformOnly: 'mac',
	},
	{
		command: 'terminal.clear',
		key: 'ctrl+shift+k',
		when: 'terminalFocus',
		source: 'default',
		label: 'Clear terminal',
		platformOnly: 'other',
	},

	// --- Pane bindings — handlers live in workspace.tsx (WP-20, do-not-touch);
	// registered here purely so labels in command-palette.tsx / new-tab-menu.tsx
	// / pane-toolbar.tsx stop hard-coding the glyphs. ---
	{
		command: 'pane.new-shell-terminal',
		key: 'ctrl+t',
		when: '!inputFocus',
		source: 'default',
		label: 'New terminal',
	},
	{
		command: 'pane.new-claude-terminal',
		key: 'ctrl+shift+t',
		when: '!inputFocus',
		source: 'default',
		label: 'New Claude terminal',
	},
	{
		command: 'pane.new-artifact',
		key: 'mod+shift+n',
		when: '!inputFocus',
		source: 'default',
		label: 'New artifact',
	},
	// `when: 'always'`, not `'!inputFocus'`: workspace.tsx's `\\` branch has
	// no `inEditable` guard, so these fire while typing too — matching the
	// shipped handler, not the guard the other pane bindings actually have.
	{
		command: 'pane.split-right',
		key: 'mod+\\',
		when: 'always',
		source: 'default',
		label: 'Split right',
	},
	{
		command: 'pane.split-down',
		key: 'mod+shift+\\',
		when: 'always',
		source: 'default',
		label: 'Split down',
	},

	// --- Workspace (src/shell/workspace.tsx, WP-20, do-not-touch) — handler
	// lives there; registered here purely as the label/conflicts source of
	// truth (DEC-26: Explorer toggle registered once, here). `when` mirrors
	// each handler's actual `inEditable` guard exactly — several of these
	// (explorer.toggle, pane.focus-N) have none, so they're `always`. ---
	{
		command: 'explorer.toggle',
		key: 'mod+b',
		when: 'always',
		source: 'default',
		label: 'Toggle Explorer',
	},
	// ⌘T → palette 'views'. Mac-only in practice: on non-Mac, `mod` resolves
	// to the literal Ctrl key, so ⌘T and the terminal's `pane.new-shell-terminal`
	// (⌃T) collide — the terminal's `ctrlOnly` branch runs first and wins
	// (workspace.tsx comment, §2). Use ⌘K there instead, same as shipped.
	{
		command: 'palette.views',
		key: 'mod+t',
		when: '!inputFocus',
		source: 'default',
		label: 'Command palette (views)',
		platformOnly: 'mac',
	},
	// ⌘⇧T → reopen last closed tab. Same non-mac collision as palette.views
	// above, for the same reason: `mod+shift+t` resolves to the literal
	// `ctrl+shift+t` the terminal's `ctrlOnly` branch already claims for
	// "new Claude terminal" (`pane.new-claude-terminal`), and that branch is
	// checked first and wins (workspace.tsx, §2).
	{
		command: 'pane.reopen',
		key: 'mod+shift+t',
		when: '!inputFocus',
		source: 'default',
		label: 'Reopen last closed tab',
		platformOnly: 'mac',
	},
	{
		command: 'palette.switcher',
		key: 'mod+shift+p',
		when: '!inputFocus',
		source: 'default',
		label: 'Open tabs switcher',
	},
	{
		command: 'pane.close',
		key: 'mod+w',
		when: '!inputFocus',
		source: 'default',
		label: 'Close pane',
	},
	{
		command: 'tab.close',
		key: 'mod+shift+w',
		when: '!inputFocus',
		source: 'default',
		label: 'Close tab',
	},
	{
		command: 'pane.focus-1',
		key: 'ctrl+1',
		when: 'always',
		source: 'default',
		label: 'Focus pane 1',
	},
	{
		command: 'pane.focus-2',
		key: 'ctrl+2',
		when: 'always',
		source: 'default',
		label: 'Focus pane 2',
	},
	{
		command: 'pane.focus-3',
		key: 'ctrl+3',
		when: 'always',
		source: 'default',
		label: 'Focus pane 3',
	},
	{
		command: 'pane.focus-4',
		key: 'ctrl+4',
		when: 'always',
		source: 'default',
		label: 'Focus pane 4',
	},
	{
		command: 'pane.focus-5',
		key: 'ctrl+5',
		when: 'always',
		source: 'default',
		label: 'Focus pane 5',
	},
	{
		command: 'pane.focus-6',
		key: 'ctrl+6',
		when: 'always',
		source: 'default',
		label: 'Focus pane 6',
	},

	// --- Companion (src/shell/companion/, WP-06) — `companion.toggle` was
	// `dock.cycle` before the Dock became the Companion; its handler still
	// lives in workspace.tsx. The three dispatch keys are handled by the
	// dispatch input's own `onKeyDown` (they only mean anything while it holds
	// focus); they are registered so the hint row reads them from here.
	// ⌘⇧A (spec §2 "focus the dispatch input") is NOT bound: on macOS it is
	// already `session.switch-adapter` (an `always` native-menu accelerator).
	{
		command: 'companion.toggle',
		key: 'mod+j',
		when: '!inputFocus',
		source: 'default',
		label: 'Toggle Companion',
	},
	{
		command: 'companion.send',
		key: 'enter',
		when: 'always',
		source: 'default',
		label: 'Companion → send to target',
	},
	{
		command: 'companion.new-run',
		key: 'shift+enter',
		when: 'always',
		source: 'default',
		label: 'Companion → start a new run',
	},
	{
		command: 'companion.persistent-run',
		key: 'alt+enter',
		when: 'always',
		source: 'default',
		label: 'Companion → start a persistent run',
	},

	// --- Native menu (src/shell/native-menu.ts) — macOS-only. These are OS
	// Menu accelerators, not a DOM `keydown` listener: the platform menu bar
	// fires them regardless of which element has focus in the webview, so
	// `when: 'always'` (not `'!inputFocus'`) is what actually ships. Under
	// DEC-59, `menu.new-terminal` (⌘T, `always`) vs `palette.views` (⌘T,
	// `!inputFocus`) is *precedence* — listed by `conflicts()` separately,
	// never a clash. Today both fire (the OS accelerator does not stop the DOM
	// listener); DEC-58's accelerator dedupe and the DEC-64 re-key land in
	// WP-54.
	{
		command: 'menu.new-session',
		key: 'mod+n',
		when: 'always',
		source: 'default',
		label: 'New Session',
		platformOnly: 'mac',
		knownOverlap: ['ngwa.create'],
	},
	{
		command: 'menu.open-file',
		key: 'mod+o',
		when: 'always',
		source: 'default',
		label: 'Open File…',
		platformOnly: 'mac',
	},
	{
		command: 'menu.open-project-folder',
		key: 'mod+shift+o',
		when: 'always',
		source: 'default',
		label: 'Open Project Folder…',
		platformOnly: 'mac',
	},
	{
		command: 'menu.new-terminal',
		key: 'mod+t',
		when: 'always',
		source: 'default',
		label: 'New Terminal (menu)',
		platformOnly: 'mac',
		knownOverlap: ['palette.views'],
	},
	{
		command: 'session.switch-adapter',
		key: 'mod+shift+a',
		when: 'always',
		source: 'default',
		label: 'Switch Adapter (coming soon)',
		platformOnly: 'mac',
	},

	// --- WP-09 frame chrome: the Shortcuts view (§2 `?`, §6A.5 `⌘/`). Both
	// open the ⌘K palette on its grouped Shortcuts view (v4 P11 — one overlay,
	// one registry). Listeners live in `useCommandPalette()`
	// (command-palette.tsx), next to `palette.open`'s, because the palette's
	// open state is owned there. `?` is matched on `e.key` with Shift ignored
	// (it needs Shift on some layouts and not on others), so it is not wired
	// through `useKey()`; both are `!inputFocus` — `?` must type a literal `?`
	// into a text field, and `⌘/` is "toggle comment" inside code editors. ---
	{
		command: 'shortcuts.open',
		key: 'mod+/',
		when: '!inputFocus',
		source: 'default',
		label: 'Keyboard shortcuts',
	},
	{
		command: 'shortcuts.open-quick',
		key: '?',
		when: '!inputFocus',
		source: 'default',
		label: 'Keyboard shortcuts (outside text fields)',
	},
];
