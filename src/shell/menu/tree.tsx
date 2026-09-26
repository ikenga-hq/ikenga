// WP-46 — the one native-menu tree (D-08 `native-menu` / `native-menu-win`,
// `drafts/design-spec-D-03-07.md` §D-08). `native-menu.ts` (macOS, real OS
// menu bar via `@tauri-apps/api/menu`) and `menu/cascade.tsx` (Windows/Linux,
// the ≡ button's in-app cascading menu) both render THIS data — one
// definition, two renderers, per WP-46's brief.
//
// Every leaf that fires a real action reads its shown key through the WP-08
// registry (`findEntry` / `labelFor` / `toAccelerator`) — nothing here
// hard-codes a combo. **No new key bindings ship with this tree** (Phase 6
// owns rebinding, per WP-46's DoD): a leaf either reuses an existing
// `DEFAULT_KEYMAP` command id (so its key is whatever that command is already
// bound to) or has none and renders with no key at all.
//
// `shipped: true` marks an item (or its direct equivalent) that existed in
// `native-menu.ts` before this file — see that file's header for the exact
// shipped/added tally used in the PR body.

import { findEntry, labelFor } from '@/lib/keymap/registry';
import { isMacPlatform, toAccelerator } from '@/lib/keymap/platform';
import { modeForRoute } from '@/lib/shell/mode-routes';
import { useShellStore } from '@/lib/shell/shell-store';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useCompanionStore } from '@/shell/companion/companion-store';
import { createClaudeTerminalSession, createTerminalSession } from '@/terminal/single-terminal';
import { openCommandPalette } from '@/shell/command-palette';

/** OS-predefined items — macOS renders these via Tauri's `PredefinedMenuItem`
 *  (native behaviour, no JS action needed); `menu/cascade.tsx` renders a
 *  best-effort equivalent for Windows/Linux, where no such native menu
 *  exists to delegate to (§D-08 file note: "Windows and Linux get no native
 *  menu at all today"). */
export type PredefinedKind =
	| 'about'
	| 'hide'
	| 'quit'
	| 'undo'
	| 'redo'
	| 'cut'
	| 'copy'
	| 'paste'
	| 'selectAll'
	| 'minimize'
	| 'maximize'
	| 'fullscreen';

export interface MenuLeaf {
	kind: 'item';
	/** Stable id, unique within its menu — used for the mac `MenuItem.new({id})`
	 *  and the cascade's React key. */
	id: string;
	label: string;
	/** WP-08 registry command id. When present, BOTH renderers read the shown
	 *  key from here (`toAccelerator` / `labelFor`) instead of a literal. */
	commandId?: string;
	/** Existed (or had a direct equivalent) in `native-menu.ts` before WP-46. */
	shipped: boolean;
	/** OS-predefined behaviour — see `PredefinedKind` above. */
	predefined?: PredefinedKind;
	/** Real, already-shipped action to run on activation — a store call or
	 *  navigation, never a newly-invented dead `CustomEvent` (WP-08's own
	 *  DoD deleted two of those; see `native-menu.ts` header for the ones
	 *  already shipped that this file deliberately leaves untouched).
	 *  Omitted for items with no real, in-scope handler today — the PR body
	 *  lists these as structure-only. */
	action?: () => void;
	/** True when the item only makes sense on macOS (e.g. "Hide", which has
	 *  no Windows/Linux equivalent) — `menu/cascade.tsx` skips these. */
	macOnly?: boolean;
}

export interface MenuSeparator {
	kind: 'separator';
}

export type MenuEntry = MenuLeaf | MenuSeparator;

export interface MenuDef {
	id: string;
	label: string;
	/** Ikenga app menu renders bold on mac (matches every other Mac app's
	 *  first menu). No effect in the cascade. */
	bold?: boolean;
	items: MenuEntry[];
}

const sep: MenuSeparator = { kind: 'separator' };

function item(def: Omit<MenuLeaf, 'kind'>): MenuLeaf {
	return { kind: 'item', ...def };
}

/** Navigate the focused pane to `path`, re-syncing `activeMode` first via the
 *  same route→mode map programmatic navigation is documented to use
 *  (`mode-routes.ts`) — mirrors what a rail click does, without reaching into
 *  `activity-bar.tsx`'s do-not-touch `enterMode()` (which also carries
 *  sidebar-collapse and Companion-focus side effects out of scope here). */
export function goto(path: string): void {
	const mode = modeForRoute(path);
	if (mode) useShellStore.getState().setActiveMode(mode);
	usePaneStore.getState().navigateFocused(path);
}

/** New Session (File) / New Terminal (Chi) both create a real session tab in
 *  the focused pane — the same call `workspace.tsx`'s ⌃T / ⌃⇧T branch makes. */
function newSessionTab(engine: 'terminal' | 'claude'): void {
	const sessionId = engine === 'claude' ? createClaudeTerminalSession() : createTerminalSession();
	const focusedId = usePaneStore.getState().focusedId;
	usePaneStore.getState().addTab(focusedId, { kind: 'terminal', sessionId });
}

export const MENU_TREE: MenuDef[] = [
	// Every item here is `new` — there is no app-level "Ikenga" menu in
	// today's `native-menu.ts` at all (its `Menu.new()` starts at File).
	{
		id: 'ikenga',
		label: 'Ikenga',
		bold: true,
		items: [
			item({ id: 'about', label: 'About Ikenga', shipped: false, predefined: 'about' }),
			item({
				id: 'check-updates',
				label: 'Check for Updates…',
				shipped: false,
				action: () => goto('/settings/about'),
			}),
			sep,
			item({
				id: 'settings',
				label: 'Settings…',
				commandId: 'rail.settings',
				shipped: false,
				action: () => goto('/settings/appearance'),
			}),
			sep,
			item({ id: 'hide', label: 'Hide Ikenga', shipped: false, predefined: 'hide', macOnly: true }),
			item({ id: 'quit', label: 'Quit Ikenga', shipped: false, predefined: 'quit' }),
		],
	},
	{
		id: 'file',
		label: 'File',
		items: [
			// `menu.new-session` is a pre-existing keymap id whose shipped
			// action was a `CustomEvent` nothing listens for (see
			// native-menu.ts header) — fixed here to actually start a session,
			// the same call `workspace.tsx`'s ⌃⇧T branch makes. Unaccelerated since DEC-64.
			item({
				id: 'new-session',
				label: 'New Session',
				commandId: 'menu.new-session',
				shipped: true,
				action: () => newSessionTab('claude'),
			}),
			// Unchanged — same commandId, same dead-event dispatch
			// (`native-menu.ts` still owns `EVT`); no confidently-real
			// replacement action identified (see native-menu.ts header).
			item({ id: 'open-file', label: 'Open File…', commandId: 'menu.open-file', shipped: true }),
			item({
				id: 'open-project',
				label: 'Open Project Folder…',
				commandId: 'menu.open-project-folder',
				shipped: true,
			}),
			sep,
			// "New Tab" and "Screenshot pane…" (design's File menu) have no
			// registry id and no existing app-wide action — omitted, see PR body.
			item({
				id: 'close-tab',
				label: 'Close Tab',
				commandId: 'tab.close',
				shipped: false,
				action: () => usePaneStore.getState().closeActiveTab(),
			}),
		],
	},
	{
		id: 'edit',
		label: 'Edit',
		items: [
			item({ id: 'undo', label: 'Undo', shipped: true, predefined: 'undo' }),
			item({ id: 'redo', label: 'Redo', shipped: true, predefined: 'redo' }),
			sep,
			item({ id: 'cut', label: 'Cut', shipped: true, predefined: 'cut' }),
			item({ id: 'copy', label: 'Copy', shipped: true, predefined: 'copy' }),
			item({ id: 'paste', label: 'Paste', shipped: true, predefined: 'paste' }),
			item({ id: 'select-all', label: 'Select All', shipped: true, predefined: 'selectAll' }),
		],
	},
	{
		id: 'view',
		label: 'View',
		items: [
			// `explorer.toggle` / `pane.split-right` / `pane.split-down` /
			// `companion.toggle` are registered in DEFAULT_KEYMAP today purely
			// as label/conflicts sources (DEC-26); this is their first real
			// menu-driven trigger, calling the exact store methods their own
			// `useKey()`/`workspace.tsx` listeners call.
			item({
				id: 'toggle-explorer',
				label: 'Toggle Explorer',
				commandId: 'explorer.toggle',
				shipped: false,
				action: () => useShellStore.getState().toggleSidebar(),
			}),
			item({
				id: 'command-palette',
				label: 'Command Palette',
				commandId: 'palette.open',
				// Shipped as a menu item — but see native-menu.ts header: its
				// action dispatched a `CustomEvent` nothing ever listened for.
				// Fixed here to call the same `openCommandPalette()` every
				// other piece of frame chrome (title row, status bar) uses.
				shipped: true,
				action: () => openCommandPalette('all'),
			}),
			sep,
			item({
				id: 'toggle-companion',
				label: 'Toggle Companion',
				commandId: 'companion.toggle',
				shipped: false,
				action: () => useCompanionStore.getState().cycleState(),
			}),
			item({
				id: 'split-right',
				label: 'Split Right',
				commandId: 'pane.split-right',
				shipped: false,
				action: () => usePaneStore.getState().splitFocused('horizontal'),
			}),
			item({
				id: 'split-down',
				label: 'Split Down',
				commandId: 'pane.split-down',
				shipped: false,
				action: () => usePaneStore.getState().splitFocused('vertical'),
			}),
			// "Zoom In/Out/Reset Zoom" (design's View menu) have no app-wide
			// registry id or handler — the only zoom today is per-renderer
			// (D-08 `renderers`), not a frame-level command. Omitted.
		],
	},
	{
		id: 'project',
		label: 'Project',
		items: [
			item({
				id: 'switch-project',
				label: 'Switch Project…',
				commandId: 'palette.projects',
				shipped: false,
				action: () => openCommandPalette('projects'),
			}),
			sep,
			item({
				id: 'project-settings',
				label: 'Project Settings…',
				shipped: false,
				action: () => goto('/settings/projects'),
			}),
			// "Open Recent" and "Reveal in Files" (design's Project menu) have
			// no existing app-wide action — `artifacts.tsx`'s own "Reveal in
			// Files" row is itself a stub (`run: () => {}`). "Reset Layout" has
			// no reset entry point in `layout-state.ts` today. All three omitted.
		],
	},
	{
		// Renamed from the shipped "Session" menu (design-spec D-08 note).
		id: 'chi',
		label: 'Chi',
		items: [
			// Same dead-event-to-real-action fix as File → New Session above,
			// using the plain-terminal branch of the same helper.
			item({
				id: 'new-terminal',
				label: 'New Terminal',
				commandId: 'menu.new-terminal',
				shipped: true,
				action: () => newSessionTab('terminal'),
			}),
			// Unchanged — genuinely a not-yet-built feature ("coming soon"),
			// not a bug to fix.
			item({
				id: 'switch-adapter',
				label: 'Switch Adapter (coming soon)',
				commandId: 'session.switch-adapter',
				shipped: true,
			}),
			sep,
			item({
				id: 'permission-inbox',
				label: 'Permission Inbox',
				shipped: false,
				action: () => goto('/outbox/approvals'),
			}),
			item({ id: 'runs', label: 'Runs', shipped: false, action: () => goto('/automations?view=runs') }),
			// "Dispatch…" (design's Chi menu) would need the rail's `enterMode()`
			// side effects (sidebar collapse + focusCompanion, activity-bar.tsx,
			// do-not-touch) to behave as the label promises — omitted rather
			// than shipping a half-behaving click.
		],
	},
	{
		id: 'ngwa',
		label: 'Ngwa',
		items: [
			item({ id: 'installed', label: 'Installed', shipped: false, action: () => goto('/ngwa/installed') }),
			item({ id: 'store', label: 'Store', shipped: false, action: () => goto('/ngwa/store') }),
			item({ id: 'health', label: 'Health', shipped: false, action: () => goto('/ngwa/health') }),
			sep,
			// "Install from folder…" is the closest existing surface to
			// `ngwa.create` (the in-shell scaffolding wizard) — reused rather
			// than left unbound. ⌘N is `ngwa.create`'s alone (DEC-64: File →
			// New Session lost it and stays unaccelerated).
			item({
				id: 'install-from-folder',
				label: 'Install from folder…',
				commandId: 'ngwa.create',
				shipped: false,
				action: () => goto('/ngwa/create'),
			}),
			// "Check for package updates" has no existing action — /ngwa/health
			// has no "updates" section (only violations/sidecars/cron/data/
			// engines) so it isn't a fair stand-in. Omitted.
		],
	},
	{
		id: 'window',
		label: 'Window',
		items: [
			item({ id: 'minimize', label: 'Minimize', shipped: true, predefined: 'minimize' }),
			item({ id: 'maximize', label: 'Maximize', shipped: true, predefined: 'maximize' }),
			item({ id: 'fullscreen', label: 'Fullscreen', shipped: true, predefined: 'fullscreen' }),
		],
	},
	{
		id: 'help',
		label: 'Help',
		items: [
			// Unchanged — already real (external link / mailto).
			item({ id: 'docs', label: 'Docs', shipped: true }),
			item({ id: 'feedback', label: 'Send Feedback', shipped: true }),
			sep,
			item({
				id: 'shortcuts',
				label: 'Keyboard Shortcuts',
				commandId: 'shortcuts.open',
				shipped: false,
				action: () => openCommandPalette('shortcuts'),
			}),
			// "Cultural attribution" (design's Help menu) has no existing
			// content/route to open — omitted.
		],
	},
];

/** Mac accelerator string (`CmdOrCtrl+Shift+X`) for a leaf's `commandId`, or
 *  `undefined` for a leaf with none / an unresolved id (menu construction
 *  never throws on a stale id). */
export function macAccelerator(commandId: string | undefined): string | undefined {
	if (!commandId) return undefined;
	const entry = findEntry(commandId);
	return entry ? toAccelerator(entry.key) : undefined;
}

/** Human-readable key hint for a leaf's `commandId` (⌘ glyphs on macOS,
 *  spelled-out Ctrl/Alt/Shift elsewhere — `labelFor` already branches on the
 *  live platform), or `''` for a leaf with none.
 *
 *  `findEntry`'s fallback (used by `macAccelerator` and the native macOS
 *  menu, where it's always correct) returns the *other* platform's entry
 *  when the command has no candidate for the live one — appropriate for a
 *  mac-only accelerator lookup, wrong here: the Windows/Linux cascade must
 *  never show a key hint for a binding that's actually restricted to macOS
 *  (WP-46-F0). So this checks the resolved entry's own `platformOnly`
 *  against the live platform and renders no hint rather than a phantom one;
 *  a real non-mac binding for the same command still resolves normally. */
export function cascadeKeyLabel(commandId: string | undefined, opts?: { mac?: boolean }): string {
	if (!commandId) return '';
	const mac = opts?.mac ?? isMacPlatform();
	const entry = findEntry(commandId, { mac });
	if (!entry) return '';
	if (entry.platformOnly && entry.platformOnly !== (mac ? 'mac' : 'other')) return '';
	return labelFor(commandId, { mac });
}

/** Every `commandId` used anywhere in `MENU_TREE` — the seam `tree.test.ts`
 *  uses to prove each one resolves in the WP-08 registry (self-verifiable
 *  DoD: no menu item claims a key the registry doesn't actually have). */
export function allMenuCommandIds(): string[] {
	const ids: string[] = [];
	for (const menu of MENU_TREE) {
		for (const entry of menu.items) {
			if (entry.kind === 'item' && entry.commandId) ids.push(entry.commandId);
		}
	}
	return ids;
}
