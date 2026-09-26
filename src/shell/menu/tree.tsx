// WP-55 (menus render from data, replacing WP-46's static `MENU_TREE`) — the
// one native-menu skeleton (D-08 `native-menu` / `native-menu-win`).
// `native-menu.ts` (macOS, the real OS menu bar) and `menu/cascade.tsx`
// (Windows/Linux, the ≡ button's in-app cascading menu) both render this
// data: one definition, two renderers.
//
// Every action leaf (everything that isn't a predefined OS role, §9.3) now
// renders from the effective model — `getEffectiveMenu('native/<top>')`
// (G-ACTIONS §1.3, §10.4): order, hidden ids, and package/personal/project
// appends already applied by the merge (WP-52's `menus.ts`). `MENU_TREE`
// itself only says where that data-driven block sits relative to the
// predefined roles, which have no id in the effective model and are never
// reordered (§1.3: "predefined OS roles are fixed", §9.3): `leading` before
// it, `trailing` after. In every one of the nine menus the two never
// interleave — a menu is either all predefined (Edit, Window) or all
// data-driven (File, View, Project, Chi, Ngwa, Help), except Ikenga (About
// leads; Hide/Quit trail). `resolveMenuTree` builds the one combined,
// separator-collapsed list both renderers walk.
//
// A default `native/<top>` menu carries no separators of its own (`menus.ts`
// §1.3) — reordering/hiding through `actions.json` can still add one
// (`"---"` in a `MenuOverride.items`). The shipped separators inside a
// same-kind run (e.g. between "Check for Updates…" and "Settings…") are lost
// to this — a renderer property, not a data one (see the PR body).

import { getEffectiveMenu } from '@/lib/actions/store';
import { findEntry, labelFor } from '@/lib/keymap/registry';
import { runMenuAction } from './resolve';
import { isMacPlatform, toAccelerator } from '@/lib/keymap/platform';
import { modeForRoute } from '@/lib/shell/mode-routes';
import { useShellStore } from '@/lib/shell/shell-store';
import { usePaneStore } from '@/lib/panes/pane-store';
import { createClaudeTerminalSession, createTerminalSession } from '@/terminal/single-terminal';

/** OS-predefined items — macOS renders these via Tauri's `PredefinedMenuItem`
 *  (native behaviour, no JS action needed); `menu/cascade.tsx` renders a
 *  best-effort equivalent for Windows/Linux, where no such native menu
 *  exists to delegate to. Every role leaf in `MENU_TREE` is one of these —
 *  every non-predefined leaf is a canonical action id instead (§10.4). */
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

export interface MenuRole {
	kind: 'item';
	/** Stable id, unique within its menu — the mac `MenuItem.new({id})` and
	 *  the cascade's React key. */
	id: string;
	label: string;
	predefined: PredefinedKind;
	/** True only for "Hide" — no Windows/Linux analogue (`menu/cascade.tsx`
	 *  skips these). */
	macOnly?: boolean;
}

export interface MenuSeparator {
	kind: 'separator';
}

export type MenuEntry = MenuRole | MenuSeparator;

export interface MenuDef {
	id: string;
	label: string;
	/** Ikenga app menu renders bold on mac (matches every other Mac app's
	 *  first menu). No effect in the cascade. */
	bold?: boolean;
	/** Predefined roles before the data-driven action block. */
	leading: MenuEntry[];
	/** Predefined roles after it. */
	trailing: MenuEntry[];
}

/** One resolved leaf of the data-driven action block — a canonical id
 *  (G-ACTIONS §10) with its effective name and danger flag. */
export interface MenuActionLeaf {
	kind: 'item';
	source: 'action';
	id: string;
	label: string;
	danger: boolean;
}

export type ResolvedMenuEntry = (MenuRole & { source: 'role' }) | MenuActionLeaf | MenuSeparator;

const sep: MenuSeparator = { kind: 'separator' };

function role(def: Omit<MenuRole, 'kind'>): MenuRole {
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

const EVT = {
	openFile: 'cmd:open-file',
	openProjectFolder: 'cmd:open-project-folder',
	switchAdapter: 'cmd:switch-adapter',
} as const;

function emit(name: string) {
	window.dispatchEvent(new CustomEvent(name));
}

/** Canonical action ids with no other owner in the app (workspace.tsx, the
 *  rail, the palette and `zoom.*`/`terminal.*` already own the rest, per
 *  `commands.ts`'s header) — the native menu / cascade is their one caller,
 *  so it runs them directly rather than through a `registerCommand` no one
 *  else would ever exercise. Every other action leaf falls back to
 *  `runCommand` (WP-53's runner / WP-54's command table), so a package or
 *  user action placed at `native/<top>` (§1.3) still fires correctly. */
const NATIVE_ONLY_ACTIONS: Readonly<Record<string, () => void>> = {
	'menu.new-session': () => newSessionTab('claude'),
	'menu.open-file': () => emit(EVT.openFile),
	'menu.open-project-folder': () => emit(EVT.openProjectFolder),
	'menu.new-terminal': () => newSessionTab('terminal'),
	'session.switch-adapter': () => emit(EVT.switchAdapter),
	'project.project-settings': () => goto('/settings/projects'),
	'chi.permission-inbox': () => goto('/outbox/approvals'),
	'chi.runs': () => goto('/automations?view=runs'),
	'ngwa.installed': () => goto('/ngwa/installed'),
	'ngwa.store': () => goto('/ngwa/store'),
	'ngwa.health': () => goto('/ngwa/health'),
	'ikenga.check-updates': () => goto('/settings/about'),
	'help.docs': () => {
		window.open('https://royalti.io/docs', '_blank');
	},
	'help.feedback': () => {
		window.open('mailto:feedback@royalti.io?subject=Royalti%20PA%20Feedback', '_blank');
	},
	// `explorer.toggle` / `companion.toggle` / `pane.split-right` /
	// `pane.split-down` are owned by `workspace.tsx`'s `useCommands` (real
	// store calls); `palette.open` / `palette.projects` / `shortcuts.open` by
	// `command-palette.tsx`; `rail.settings` / `ngwa.create` by the rail. All
	// reached the same way — through `runCommand`, below — once `workspace.tsx`
	// mounts (every window that shows a native menu also mounts it).
};

/** Activate a canonical action id — a local handler for the ids nothing else
 *  in the app owns, else `runMenuAction`: a built-in through the WP-54
 *  command table (its registered owner), a personal / project action through
 *  the WP-53 runner with its outcome surfaced, a package action through its
 *  own run. Both renderers call this for every non-`predefined` leaf. */
export function activateActionId(id: string): void {
	const local = NATIVE_ONLY_ACTIONS[id];
	if (local) {
		local();
		return;
	}
	runMenuAction(id);
}

export const MENU_TREE: MenuDef[] = [
	{
		id: 'ikenga',
		label: 'Ikenga',
		bold: true,
		leading: [role({ id: 'about', label: 'About Ikenga', predefined: 'about' })],
		trailing: [
			role({ id: 'hide', label: 'Hide Ikenga', predefined: 'hide', macOnly: true }),
			role({ id: 'quit', label: 'Quit Ikenga', predefined: 'quit' }),
		],
	},
	{ id: 'file', label: 'File', leading: [], trailing: [] },
	{
		id: 'edit',
		label: 'Edit',
		leading: [
			role({ id: 'undo', label: 'Undo', predefined: 'undo' }),
			role({ id: 'redo', label: 'Redo', predefined: 'redo' }),
			sep,
			role({ id: 'cut', label: 'Cut', predefined: 'cut' }),
			role({ id: 'copy', label: 'Copy', predefined: 'copy' }),
			role({ id: 'paste', label: 'Paste', predefined: 'paste' }),
			role({ id: 'select-all', label: 'Select All', predefined: 'selectAll' }),
		],
		trailing: [],
	},
	{ id: 'view', label: 'View', leading: [], trailing: [] },
	{ id: 'project', label: 'Project', leading: [], trailing: [] },
	{ id: 'chi', label: 'Chi', leading: [], trailing: [] },
	{ id: 'ngwa', label: 'Ngwa', leading: [], trailing: [] },
	{
		id: 'window',
		label: 'Window',
		leading: [
			role({ id: 'minimize', label: 'Minimize', predefined: 'minimize' }),
			role({ id: 'maximize', label: 'Maximize', predefined: 'maximize' }),
			role({ id: 'fullscreen', label: 'Fullscreen', predefined: 'fullscreen' }),
		],
		trailing: [],
	},
	{ id: 'help', label: 'Help', leading: [], trailing: [] },
];

/** The data-driven action block for one `native/<top>` menu (§1.3: "list
 *  their action leaves only... interleaved by the renderer"). Pure — reads
 *  the effective model directly, no hook, so `native-menu.ts`'s imperative
 *  Tauri-menu rebuild can call it as readily as a React render. */
export function resolveActionBlock(menuId: string): (MenuActionLeaf | MenuSeparator)[] {
	const menu = getEffectiveMenu(menuId);
	if (!menu) return [];
	const out: (MenuActionLeaf | MenuSeparator)[] = [];
	for (const entry of menu.items) {
		out.push(
			entry.kind === 'separator'
				? { kind: 'separator' }
				: {
						kind: 'item',
						source: 'action',
						id: entry.id,
						label: entry.action.name || entry.id,
						danger: entry.action.danger,
					}
		);
	}
	return out;
}

function collapseResolvedSeparators(entries: ResolvedMenuEntry[]): ResolvedMenuEntry[] {
	const out: ResolvedMenuEntry[] = [];
	for (const e of entries) {
		if (e.kind === 'separator' && (out.length === 0 || out[out.length - 1].kind === 'separator')) continue;
		out.push(e);
	}
	while (out.length > 0 && out[out.length - 1].kind === 'separator') out.pop();
	return out;
}

/** One top menu's full leaf list: the static role skeleton around the
 *  data-driven action block, separators collapsed. */
export function resolveMenuTree(menu: MenuDef): ResolvedMenuEntry[] {
	const actions = resolveActionBlock(`native/${menu.id}`);
	const out: ResolvedMenuEntry[] = menu.leading.map((e) =>
		e.kind === 'separator' ? e : { ...e, source: 'role' as const }
	);
	if (actions.length > 0) {
		if (out.length > 0 && out[out.length - 1].kind !== 'separator') out.push(sep);
		out.push(...actions);
		if (menu.trailing.length > 0) out.push(sep);
	}
	for (const e of menu.trailing) out.push(e.kind === 'separator' ? e : { ...e, source: 'role' as const });
	return collapseResolvedSeparators(out);
}

/** Mac accelerator string (`CmdOrCtrl+Shift+X`) for a canonical action id, or
 *  `undefined` for one with no binding (menu construction never throws on a
 *  stale id). */
export function macAccelerator(commandId: string | undefined): string | undefined {
	if (!commandId) return undefined;
	const entry = findEntry(commandId);
	return entry ? toAccelerator(entry.key) : undefined;
}

/** Human-readable key hint for a canonical action id (⌘ glyphs on macOS,
 *  spelled-out Ctrl/Alt/Shift elsewhere — `labelFor` already branches on the
 *  live platform), or `''` for one with none.
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
