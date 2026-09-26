// Default bindings — the `default` layer of the registry (G-ACTIONS §2.1).
// The package / personal / project layers (manifest key requests,
// `~/.ikenga/keybindings.json`, `<project>/.ikenga/keybindings.json`) are
// merged on top of this by the effective model (WP-52); nothing here reads
// them.
//
// Every frame key fires through the one dispatcher (`dispatcher.ts`, WP-54,
// DEC-56): it resolves key → chord → `when` → command over the effective
// keymap, and a command table (`commands.ts`) maps each command to the
// handler its owner registered. Two kinds of command are the exception:
// - **hosted** (§4.6) — `terminal.*` (the xterm hook) and the three Companion
//   dispatch keys (the dispatch input). Their keys live here, so they are
//   rebindable and visible to `conflicts()`, but their owner's own listener
//   fires them through `resolveHostedKeypress()`;
// - **OS-wide** (`scope: 'os'`, §6, DEC-60) — registered with
//   `tauri-plugin-global-shortcut` by `lib.rs` from the effective default +
//   personal OS rules (`syncOsShortcuts()`), never dispatched in the webview.
//
// `when` values are DEC-62 expressions (`when.ts`). WP-54 applied the DEC-64
// re-keys (§2.4 — no default key is shared by two commands any more), the
// `dispatchFocus` Companion keys, the palette close keys, the missing
// defaults of §10.2 and the three `os.*` entries.

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
	 *  `!inputFocus` is the "no focus condition" default. OS rules carry
	 *  `always` (their `when` is ignored, §6). */
	when: WhenClause;
	source: KeymapSource;
	/** Default `'app'`. `'os'` only in the default and personal layers. */
	scope?: KeymapScope;
	/** Human label for the Shortcuts view / `?` overlay. */
	label: string;
	/** Restricts a binding to one platform family (the `platform` field of
	 *  `keybindings.json`, §1.5) — used where the real binding differs by
	 *  platform, not just by how `mod` resolves (the terminal table, pane
	 *  focus after DEC-64, `os.summon`), and for the mac-only ⌘T / ⌘⇧T keys
	 *  whose `mod` form would land on a terminal key elsewhere. */
	platformOnly?: 'mac' | 'other';
	/** Set on `personal` / `project` rules merged from a file (WP-52). */
	origin?: KeymapRuleOrigin;
	/** Set on a `package` rule: the pkg whose key request was granted
	 *  (G-ACTIONS §7.4). */
	pkgId?: string;
}

/** `pane.focus-1..6` (DEC-64, §2.4): ⌃1–⌃6 on macOS (never overlapped the
 *  rail's ⌘1–⌘3); Alt+1–6 on Windows/Linux, where the rail keeps Ctrl+1–3. */
function paneFocusEntries(): KeymapEntry[] {
	const out: KeymapEntry[] = [];
	for (let n = 1; n <= 6; n++) {
		const base = { command: `pane.focus-${n}`, when: 'always', source: 'default' as const, label: `Focus pane ${n}` };
		out.push({ ...base, key: `ctrl+${n}`, platformOnly: 'mac' });
		out.push({ ...base, key: `alt+${n}`, platformOnly: 'other' });
	}
	return out;
}

/** The terminal's own table (TK, `terminal/keybindings.ts` before WP-54) as
 *  hosted `terminal.*` commands (§10.2): ⌘-based on macOS, Ctrl+Shift-based
 *  elsewhere so plain Ctrl+C / Ctrl+V stay SIGINT / literal for the PTY. The
 *  xterm hook reads these (one grammar, DEC-56). */
function terminalEntries(): KeymapEntry[] {
	const rows: Array<[command: string, mac: string, other: string, label: string]> = [
		['terminal.copy', 'mod+c', 'ctrl+shift+c', 'Copy (terminal)'],
		['terminal.paste', 'mod+v', 'ctrl+shift+v', 'Paste (terminal)'],
		['terminal.find', 'mod+f', 'ctrl+shift+f', 'Find (terminal)'],
		['terminal.clear', 'mod+k', 'ctrl+shift+k', 'Clear terminal'],
		['terminal.select-all', 'mod+a', 'ctrl+shift+a', 'Select all (terminal)'],
		['terminal.prev-prompt', 'mod+arrowup', 'ctrl+arrowup', 'Previous prompt (terminal)'],
		['terminal.next-prompt', 'mod+arrowdown', 'ctrl+arrowdown', 'Next prompt (terminal)'],
	];
	const out: KeymapEntry[] = [];
	for (const [command, mac, other, label] of rows) {
		out.push({ command, key: mac, when: 'terminalFocus', source: 'default', label, platformOnly: 'mac' });
		out.push({ command, key: other, when: 'terminalFocus', source: 'default', label, platformOnly: 'other' });
	}
	return out;
}

export const DEFAULT_KEYMAP: KeymapEntry[] = [
	// --- Rail (src/shell/activity-bar.tsx) — WP-03: Project · Chi · Ngwa ·
	// Settings (spec §2). ⌘4–⌘6 are retired and deliberately left unbound this
	// release (§2 "Retired", §7 Q2). On Windows/Linux the rail keeps Ctrl+1–3
	// (DEC-64); pane focus moved to Alt+1–6 there.
	{ command: 'rail.project', key: 'mod+1', when: '!inputFocus', source: 'default', label: 'Rail → Project' },
	{ command: 'rail.chi', key: 'mod+2', when: '!inputFocus', source: 'default', label: 'Rail → Chi' },
	{ command: 'rail.ngwa', key: 'mod+3', when: '!inputFocus', source: 'default', label: 'Rail → Ngwa' },
	{ command: 'rail.settings', key: 'mod+,', when: '!inputFocus', source: 'default', label: 'Rail → Settings' },

	// --- Command palette (src/shell/command-palette.tsx registers these).
	// `palette.open` / `shortcuts.open` and `palette.close` /
	// `palette.toggle-shortcuts` share a key with mutually exclusive `when`s
	// (§4.6): the close keys fire from inside the palette's own search input,
	// which is itself a typing target. ⌘K is also `terminal.clear` while the
	// terminal has focus (hosted, precedence) and becomes a chord prefix only
	// while a ⌘K chord is bound (DEC-57). Escape stays widget-local.
	{
		command: 'palette.open',
		key: 'mod+k',
		when: '!inputFocus && !paletteOpen',
		source: 'default',
		label: 'Command palette',
	},
	{ command: 'palette.close', key: 'mod+k', when: 'paletteOpen', source: 'default', label: 'Close command palette' },
	{ command: 'palette.projects', key: 'mod+p', when: '!inputFocus', source: 'default', label: 'Project switcher' },
	{
		command: 'palette.switcher',
		key: 'mod+shift+p',
		when: '!inputFocus',
		source: 'default',
		label: 'Open tabs switcher',
	},
	// ⌘T → palette 'views', macOS only: `mod+t` resolves to the literal Ctrl+T
	// elsewhere, which is `pane.new-shell-terminal`. Sole owner of ⌘T on macOS
	// since DEC-64 unbound the native Chi → New Terminal accelerator.
	{
		command: 'palette.views',
		key: 'mod+t',
		when: '!inputFocus',
		source: 'default',
		label: 'Command palette (views)',
		platformOnly: 'mac',
	},

	// --- Ngwa create (WP-26) — sole owner of ⌘N / Ctrl+N (DEC-64: the native
	// File → New Session item lost its ⌘N and stays unaccelerated).
	{ command: 'ngwa.create', key: 'mod+n', when: '!inputFocus', source: 'default', label: 'Ngwa → Create' },

	// --- Terminal (hosted, §4.6) ---
	...terminalEntries(),

	// --- Pane and tab (src/shell/workspace.tsx registers these) ---
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
	{ command: 'pane.new-artifact', key: 'mod+shift+n', when: '!inputFocus', source: 'default', label: 'New artifact' },
	// `always`: the shipped `\\` branch had no typing guard.
	{ command: 'pane.split-right', key: 'mod+\\', when: 'always', source: 'default', label: 'Split right' },
	{ command: 'pane.split-down', key: 'mod+shift+\\', when: 'always', source: 'default', label: 'Split down' },
	// ⌘⇧T, macOS only for the same reason as `palette.views`: elsewhere
	// `mod+shift+t` is Ctrl+Shift+T, `pane.new-claude-terminal`.
	{
		command: 'pane.reopen',
		key: 'mod+shift+t',
		when: '!inputFocus',
		source: 'default',
		label: 'Reopen last closed tab',
		platformOnly: 'mac',
	},
	{ command: 'pane.close', key: 'mod+w', when: '!inputFocus', source: 'default', label: 'Close pane' },
	{ command: 'tab.close', key: 'mod+shift+w', when: '!inputFocus', source: 'default', label: 'Close tab' },
	...paneFocusEntries(),
	// Missing defaults from spec §2 (§10.2, added by WP-54).
	{
		command: 'pane.tab-prev',
		key: 'mod+alt+arrowleft',
		when: 'paneFocus',
		source: 'default',
		label: 'Previous tab',
	},
	{ command: 'pane.tab-next', key: 'mod+alt+arrowright', when: 'paneFocus', source: 'default', label: 'Next tab' },
	{
		command: 'pane.focus-up',
		key: 'mod+alt+arrowup',
		when: '!inputFocus',
		source: 'default',
		label: 'Move pane focus up',
	},
	{
		command: 'pane.focus-down',
		key: 'mod+alt+arrowdown',
		when: '!inputFocus',
		source: 'default',
		label: 'Move pane focus down',
	},

	// --- Explorer (DEC-26: toggle registered once, here). `explorer.toggle`
	// is `always` — the shipped ⌘B branch had no typing guard. ⌘. restores
	// the regressed "show hidden files" key (G-ACTIONS §15 A-6).
	{ command: 'explorer.toggle', key: 'mod+b', when: 'always', source: 'default', label: 'Toggle Explorer' },
	{
		command: 'explorer.toggle-hidden',
		key: 'mod+.',
		when: 'explorerFocus',
		source: 'default',
		label: 'Show hidden files',
	},
	// WP-56 (PR #209 leftover, off-list, G-ACTIONS §10.2/§10.6): was
	// `explorer.tsx:87`'s own keydown branch. `explorerFocus` is the existing
	// §4.3 key, already backed by `[data-explorer-section]` — no new focus
	// key needed for this one.
	{
		command: 'explorer.section-prev',
		key: 'mod+shift+[',
		when: 'explorerFocus',
		source: 'default',
		label: 'Previous Explorer section',
	},
	{
		command: 'explorer.section-next',
		key: 'mod+shift+]',
		when: 'explorerFocus',
		source: 'default',
		label: 'Next Explorer section',
	},

	// --- Companion (src/shell/companion/, WP-06). ⌘⇧A focuses the dispatch
	// input (DEC-63.2; `session.switch-adapter` moved off it to no key). The
	// three dispatch keys are hosted by the dispatch input (§4.6, A-2): they
	// fire only while it has focus (`dispatchFocus`), from its own Enter
	// handler, never from the frame dispatcher.
	{ command: 'companion.toggle', key: 'mod+j', when: '!inputFocus', source: 'default', label: 'Toggle Companion' },
	{
		command: 'companion.focus-dispatch',
		key: 'mod+shift+a',
		when: '!inputFocus',
		source: 'default',
		label: 'Focus the dispatch input',
	},
	{
		command: 'companion.send',
		key: 'enter',
		when: 'dispatchFocus',
		source: 'default',
		label: 'Companion → send to target',
	},
	{
		command: 'companion.new-run',
		key: 'shift+enter',
		when: 'dispatchFocus',
		source: 'default',
		label: 'Companion → start a new run',
	},
	{
		command: 'companion.persistent-run',
		key: 'alt+enter',
		when: 'dispatchFocus',
		source: 'default',
		label: 'Companion → start a persistent run',
	},

	// --- WP-56: PR #209's leftover widget-local handlers, migrated to
	// registry commands with scoped `when`s (G-ACTIONS §10.2 "Reserved for
	// WP-56", §10.6). Each `<area>Focus` key below is new, added additively
	// under B-21 — none changes an existing evaluation.
	{
		command: 'companion.permission-allow',
		key: 'a',
		when: 'permissionCardFocus',
		source: 'default',
		label: 'Allow the focused permission request',
	},
	// Fix round 1: the pre-registry handler matched `e.key.toLowerCase()`, so
	// Shift+A also allowed (same as plain A); this restores that (§3.1:
	// shifted-letter variant alongside the unshifted key, as `zoom.out` does
	// above for ⌘⇧-).
	{
		command: 'companion.permission-allow',
		key: 'shift+a',
		when: 'permissionCardFocus',
		source: 'default',
		label: 'Allow the focused permission request',
	},
	{
		command: 'companion.permission-deny',
		key: 'd',
		when: 'permissionCardFocus',
		source: 'default',
		label: 'Deny the focused permission request',
	},
	{
		command: 'approve-gate.next',
		key: 'j',
		when: 'approveGateFocus && !inputFocus',
		source: 'default',
		label: 'Next draft (approve gate)',
	},
	{
		command: 'approve-gate.prev',
		key: 'k',
		when: 'approveGateFocus && !inputFocus',
		source: 'default',
		label: 'Previous draft (approve gate)',
	},
	{
		command: 'approve-gate.save',
		key: 'mod+s',
		// Fix round 1: narrower than `approveGateFocus` — pre-WP-56 this only
		// fired while the detail pane had focus, not the whole section.
		when: 'approveGateDetailFocus',
		source: 'default',
		label: 'Save draft (approve gate)',
	},
	{
		command: 'approve-gate.approve',
		key: 'mod+enter',
		when: 'approveGateDetailFocus',
		source: 'default',
		label: 'Approve & send (approve gate)',
	},
	{
		command: 'studio.loupe-save',
		key: 'mod+s',
		when: 'loupeFocus',
		source: 'default',
		label: 'Save (Studio loupe)',
	},
	{
		command: 'studio.pin-submit',
		key: 'mod+enter',
		when: 'pinComposerFocus',
		source: 'default',
		label: 'Add pin (pin composer)',
	},
	{
		command: 'markdown.save',
		key: 'mod+s',
		when: 'markdownEditorFocus',
		source: 'default',
		label: 'Save (markdown editor)',
	},
	// Precedence, not a clash (DEC-59, §2.3): `explorer.toggle` also holds
	// `mod+b` with `when: 'always'` (specificity 0). `markdownEditorFocus`
	// (specificity 1) outranks it, so this wins while the markdown editor has
	// focus and `explorer.toggle` wins everywhere else — no re-key needed.
	{
		command: 'markdown.bold',
		key: 'mod+b',
		when: 'markdownEditorFocus',
		source: 'default',
		label: 'Bold (markdown editor)',
	},
	{
		command: 'markdown.italic',
		key: 'mod+i',
		when: 'markdownEditorFocus',
		source: 'default',
		label: 'Italic (markdown editor)',
	},

	// --- Native menu (src/shell/native-menu.ts) — macOS-only accelerators.
	// The OS menu fires them regardless of webview focus, hence `always`; the
	// item handler is deduplicated against the dispatcher (DEC-58) so each
	// press runs once. `menu.new-session` (⌘N), `menu.new-terminal` (⌘T) and
	// `session.switch-adapter` (⌘⇧A) have no key any more (DEC-64, DEC-63.2):
	// their `MENU_TREE` leaves stay, unaccelerated.
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

	// --- Shortcuts view (§2 `?`, §6A.5 `⌘/`) — both open the ⌘K palette on
	// its grouped Shortcuts view. `?` is a character key matched with Shift
	// ignored (§3.1) and toggles the view, as shipped; `⌘/` opens it, and
	// `palette.toggle-shortcuts` flips the open palette between Shortcuts and
	// All from inside its own input.
	{
		command: 'shortcuts.open',
		key: 'mod+/',
		when: '!inputFocus && !paletteOpen',
		source: 'default',
		label: 'Keyboard shortcuts',
	},
	{
		command: 'palette.toggle-shortcuts',
		key: 'mod+/',
		when: 'paletteOpen',
		source: 'default',
		label: 'Toggle shortcuts view',
	},
	{
		command: 'shortcuts.open-quick',
		key: '?',
		when: '!inputFocus',
		source: 'default',
		label: 'Keyboard shortcuts (outside text fields)',
	},

	// --- Window zoom (`lib/window/zoom.ts`) — `always`: "make everything
	// bigger" is meaningful while typing. `mod+plus` keeps the shipped
	// matcher's `+` / numpad Add (Shift+= on US layouts).
	{ command: 'zoom.in', key: 'mod+=', when: 'always', source: 'default', label: 'Zoom in' },
	{ command: 'zoom.in', key: 'mod+plus', when: 'always', source: 'default', label: 'Zoom in' },
	{ command: 'zoom.out', key: 'mod+-', when: 'always', source: 'default', label: 'Zoom out' },
	// Round 42 hand-off (WP-54 review): the pre-registry handler matched
	// `e.key === '-' || '_' || 'Subtract'` under `mod`, so ⌘⇧- (which produces
	// `_` on a US layout) also zoomed out. The registry row above only ever
	// carried `mod+-`; this restores the shifted variant (§3.1: shifted
	// punctuation names the unshifted key).
	{ command: 'zoom.out', key: 'mod+shift+-', when: 'always', source: 'default', label: 'Zoom out' },
	{ command: 'zoom.reset', key: 'mod+0', when: 'always', source: 'default', label: 'Reset zoom' },

	// --- OS-wide (§6, DEC-60) — registered by `lib.rs` from the effective
	// default + personal OS rules, re-registered on change. `os.summon`'s
	// Windows/Linux default (Super+Space) is kept as shipped but is FLAGGED:
	// on Windows it may collide with the input-language switcher (inferred,
	// unverified — `04` Round 37 "Still open"). Not changed silently.
	{
		command: 'os.summon',
		key: 'alt+space',
		when: 'always',
		source: 'default',
		scope: 'os',
		label: 'Summon Ikenga',
		platformOnly: 'mac',
	},
	{
		command: 'os.summon',
		key: 'meta+space',
		when: 'always',
		source: 'default',
		scope: 'os',
		label: 'Summon Ikenga',
		platformOnly: 'other',
	},
	{
		command: 'os.screenshot-window',
		key: 'ctrl+alt+shift+s',
		when: 'always',
		source: 'default',
		scope: 'os',
		label: 'Screenshot window',
	},
	{
		command: 'os.screenshot-pane',
		key: 'ctrl+alt+shift+p',
		when: 'always',
		source: 'default',
		scope: 'os',
		label: 'Screenshot focused pane',
	},
];
