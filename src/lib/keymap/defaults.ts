// Phase 1 default bindings — the read-only seed of the registry. User and
// project overrides (`~/.ikenga/keybindings.json`, `<project>/.ikenga/keybindings.json`)
// are Phase 6 (§6A.5); nothing here reads them yet.
//
// Every entry is a binding that exists in the shipped app today (`source:
// 'default'`), ported from the `keydown` handlers this registry replaces as
// the label source (`workspace.tsx`, `activity-bar.tsx`) plus
// `native-menu.ts`'s accelerators. Commands whose *handler* still lives in an
// untouched file (`workspace.tsx` — owned by WP-20; `terminal/keybindings.ts`
// — do-not-touch) are still registered here so `labelFor()` and `conflicts()`
// have one source of truth for their key hints, even though nothing here
// fires them. `rail.*` gets a live `useKey()` listener wired through this
// registry (`activity-bar.tsx`). `palette.open` has its own listener
// (`command-palette.tsx`) rather than `useKey()`, because closing the palette
// must bypass the `not-input` guard that opening it observes — the palette's
// own search input is itself a typing target, and ⌘K has always closed it
// from there.

import type { WhenClause } from './when';

export interface KeymapEntry {
	/** Stable command id, e.g. `rail.app`. Namespaced by owning surface. */
	command: string;
	/** Combo string — see `platform.ts` for grammar. */
	key: string;
	when: WhenClause;
	source: 'default' | 'user' | 'project';
	/** Human label for the Shortcuts view / `?` overlay (Phase 1 doesn't ship
	 *  the overlay itself — WP-09 — but the data belongs with the binding). */
	label: string;
	/** Restricts a binding to a platform for `conflicts()` purposes — the
	 *  native menu (and a handful of mac-only combos that collide with a
	 *  terminal binding once `mod` resolves to the literal Ctrl key) only
	 *  apply on macOS; `'other'` is the mirror case (`terminal.clear`'s
	 *  non-mac chord, which is a different combo, not the same one platform-
	 *  gated). */
	platformOnly?: 'mac' | 'other';
	/**
	 * Command ids this entry is documented to fire *alongside* on the same
	 * resolved key — a real, shipped parallel-fire, not a clash `conflicts()`
	 * should report and not something to hide by fudging `when`. Checked in
	 * either direction (declaring it on one side of the pair is enough).
	 * Today's two: the mac `⌘T` OS-menu accelerator racing the in-app
	 * `not-input` listener (both fire; the menu doesn't preventDefault a DOM
	 * listener), and non-mac `Ctrl+1..6` driving both the rail and pane-focus
	 * listeners (neither calls `stopPropagation`). Both predate this
	 * registry — recorded here, not fixed, since WP-08 is behaviour-
	 * preserving (WP-03 owns any real rail remap).
	 */
	knownOverlap?: string[];
}

export const DEFAULT_KEYMAP: KeymapEntry[] = [
	// --- Rail (src/shell/activity-bar.tsx) — WP-03: Project · Chi · Ngwa ·
	// Settings (spec §2). ⌘4–⌘6 are retired and deliberately left unbound this
	// release (§2 "Retired", §7 Q2) — no entry, so nothing fires and nothing
	// can be labelled with them.
	// `knownOverlap` on rail.project..rail.ngwa: on non-mac, `mod+N` resolves
	// to the literal `ctrl+N` that `pane.focus-N` (global, workspace.tsx) is
	// also bound to. Both are live `window` keydown listeners and neither
	// calls `stopPropagation`, so a non-mac Ctrl+1 both switches the rail AND
	// focuses pane 1 — a shipped parallel-fire, not a clash.
	{
		command: 'rail.project',
		key: 'mod+1',
		when: 'not-input',
		source: 'default',
		label: 'Rail → Project',
		knownOverlap: ['pane.focus-1'],
	},
	{
		command: 'rail.chi',
		key: 'mod+2',
		when: 'not-input',
		source: 'default',
		label: 'Rail → Chi',
		knownOverlap: ['pane.focus-2'],
	},
	{
		command: 'rail.ngwa',
		key: 'mod+3',
		when: 'not-input',
		source: 'default',
		label: 'Rail → Ngwa',
		knownOverlap: ['pane.focus-3'],
	},
	{
		command: 'rail.settings',
		key: 'mod+,',
		when: 'not-input',
		source: 'default',
		label: 'Rail → Settings',
	},

	// --- Command palette (src/shell/command-palette.tsx) ---
	{
		command: 'palette.open',
		key: 'mod+k',
		when: 'not-input',
		source: 'default',
		label: 'Command palette',
	},
	// Handler lives in workspace.tsx (WP-20, do-not-touch); registered here so
	// activity-bar.tsx's project-switcher tooltip stops hard-coding "⌘P".
	{
		command: 'palette.projects',
		key: 'mod+p',
		when: 'not-input',
		source: 'default',
		label: 'Project switcher',
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
		when: 'terminal-focus',
		source: 'default',
		label: 'Clear terminal',
		platformOnly: 'mac',
	},
	{
		command: 'terminal.clear',
		key: 'ctrl+shift+k',
		when: 'terminal-focus',
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
		when: 'not-input',
		source: 'default',
		label: 'New terminal',
	},
	{
		command: 'pane.new-claude-terminal',
		key: 'ctrl+shift+t',
		when: 'not-input',
		source: 'default',
		label: 'New Claude terminal',
	},
	{
		command: 'pane.new-artifact',
		key: 'mod+shift+n',
		when: 'not-input',
		source: 'default',
		label: 'New artifact',
	},
	// `when: 'global'`, not `'not-input'`: workspace.tsx's `\\` branch has no
	// `inEditable` guard, so these fire while typing too — matching the
	// shipped handler, not the guard the other pane bindings actually have.
	{
		command: 'pane.split-right',
		key: 'mod+\\',
		when: 'global',
		source: 'default',
		label: 'Split right',
	},
	{
		command: 'pane.split-down',
		key: 'mod+shift+\\',
		when: 'global',
		source: 'default',
		label: 'Split down',
	},

	// --- Workspace (src/shell/workspace.tsx, WP-20, do-not-touch) — handler
	// lives there; registered here purely as the label/conflicts source of
	// truth (DEC-26: Explorer toggle registered once, here). `when` mirrors
	// each handler's actual `inEditable` guard exactly — several of these
	// (explorer.toggle, pane.focus-N) have none, so they're `global`. ---
	{
		command: 'explorer.toggle',
		key: 'mod+b',
		when: 'global',
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
		when: 'not-input',
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
		when: 'not-input',
		source: 'default',
		label: 'Reopen last closed tab',
		platformOnly: 'mac',
	},
	{
		command: 'palette.switcher',
		key: 'mod+shift+p',
		when: 'not-input',
		source: 'default',
		label: 'Open tabs switcher',
	},
	{
		command: 'pane.close',
		key: 'mod+w',
		when: 'not-input',
		source: 'default',
		label: 'Close pane',
	},
	{
		command: 'tab.close',
		key: 'mod+shift+w',
		when: 'not-input',
		source: 'default',
		label: 'Close tab',
	},
	{
		command: 'pane.focus-1',
		key: 'ctrl+1',
		when: 'global',
		source: 'default',
		label: 'Focus pane 1',
	},
	{
		command: 'pane.focus-2',
		key: 'ctrl+2',
		when: 'global',
		source: 'default',
		label: 'Focus pane 2',
	},
	{
		command: 'pane.focus-3',
		key: 'ctrl+3',
		when: 'global',
		source: 'default',
		label: 'Focus pane 3',
	},
	{
		command: 'pane.focus-4',
		key: 'ctrl+4',
		when: 'global',
		source: 'default',
		label: 'Focus pane 4',
	},
	{
		command: 'pane.focus-5',
		key: 'ctrl+5',
		when: 'global',
		source: 'default',
		label: 'Focus pane 5',
	},
	{
		command: 'pane.focus-6',
		key: 'ctrl+6',
		when: 'global',
		source: 'default',
		label: 'Focus pane 6',
	},
	{
		command: 'dock.cycle',
		key: 'mod+j',
		when: 'not-input',
		source: 'default',
		label: 'Cycle dock',
	},

	// --- Native menu (src/shell/native-menu.ts) — macOS-only. These are OS
	// Menu accelerators, not a DOM `keydown` listener: the platform menu bar
	// fires them regardless of which element has focus in the webview, so
	// `when: 'global'` (not `'not-input'`) is what actually ships. `global`
	// overlaps every other `when` for `conflicts()` purposes, so
	// `menu.new-terminal` (⌘T) DOES flag against `palette.views` (also ⌘T,
	// `not-input`) unless declared via `knownOverlap` below — which is the
	// honest description: the OS accelerator fires unconditionally, in
	// parallel with (not instead of) whatever the in-app `not-input`
	// listener does, and today's app ships exactly that double-fire.
	{
		command: 'menu.new-session',
		key: 'mod+n',
		when: 'global',
		source: 'default',
		label: 'New Session',
		platformOnly: 'mac',
	},
	{
		command: 'menu.open-file',
		key: 'mod+o',
		when: 'global',
		source: 'default',
		label: 'Open File…',
		platformOnly: 'mac',
	},
	{
		command: 'menu.open-project-folder',
		key: 'mod+shift+o',
		when: 'global',
		source: 'default',
		label: 'Open Project Folder…',
		platformOnly: 'mac',
	},
	{
		command: 'menu.new-terminal',
		key: 'mod+t',
		when: 'global',
		source: 'default',
		label: 'New Terminal (menu)',
		platformOnly: 'mac',
		knownOverlap: ['palette.views'],
	},
	{
		command: 'session.switch-adapter',
		key: 'mod+shift+a',
		when: 'global',
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
	// through `useKey()`; both are `not-input` — `?` must type a literal `?`
	// into a text field, and `⌘/` is "toggle comment" inside code editors. ---
	{
		command: 'shortcuts.open',
		key: 'mod+/',
		when: 'not-input',
		source: 'default',
		label: 'Keyboard shortcuts',
	},
	{
		command: 'shortcuts.open-quick',
		key: '?',
		when: 'not-input',
		source: 'default',
		label: 'Keyboard shortcuts (outside text fields)',
	},
];
