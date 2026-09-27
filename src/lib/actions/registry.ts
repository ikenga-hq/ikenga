// The action registry (WP-52): every action the effective model knows, from
// its four sources (G-ACTIONS §0, §2.1) — built-in (code), package (manifest
// `ui.context_actions[]` + `ui.command_palette[]`), personal and project
// (`actions.json`). This file holds the **static** half: the built-in
// catalog (the §10 canonical ids that are not user-definable), the locked
// set (§9.2), the package projection from the kernel snapshot, and the
// selector → `when` / placement derivations (§7.3, §7.3a, §12). The merge
// itself is `merge.ts`; menus are `menus.ts`; the store and the published
// API (G-ACTIONS-API) are `store.ts`.
//
// Built-ins run code and are never serialized (§8.1): a built-in's `run` is
// `{ kind: 'builtin' }` and whoever owns the surface (WP-54's dispatcher
// commands, WP-55's menu renderers) binds the id to behaviour. This catalog
// only names them, so menus, the Actions tab and the Keys tab can list them.

import { iconNames } from 'lucide-react/dynamic';
import { DEFAULT_KEYMAP } from '@/lib/keymap/defaults';
import { isHostedCommand } from '@/lib/keymap/registry';
import type { ActionRun, Placement, UserAction } from './types';

// ─── Ids ─────────────────────────────────────────────────────────────────────

/** Never-hide items (§9.2): reorderable within a menu, never hidden. The
 *  merge rejects hiding one (`E_LOCKED_HIDDEN`) independently of the UI. */
export const LOCKED_ACTION_IDS: ReadonlySet<string> = new Set(['delete', 'pane.close', 'tab.close', 'kill-session']);

export function isLockedAction(id: string): boolean {
	return LOCKED_ACTION_IDS.has(id);
}

/** A package action id: `${pkg_id}:${id}` — the only class containing `:`. */
export function isPackageActionId(id: string): boolean {
	return id.includes(':');
}

/** `${pkg_id}:${id}` → `pkg_id`, or null for a non-package id. */
export function pkgIdOfAction(id: string): string | null {
	const i = id.indexOf(':');
	return i > 0 ? id.slice(0, i) : null;
}

/** `os.*`: the shell's three OS-only commands (§6) — never bound in-app. */
export function isOsOnlyAction(id: string): boolean {
	return id.startsWith('os.');
}

// ─── Built-in catalog (§10.2, §10.3) ─────────────────────────────────────────

export interface BuiltinActionDef {
	id: string;
	name: string;
	/** Destructive (§9.2 "Danger"), rendered with the destructive variant. */
	danger?: boolean;
}

/** Names for built-ins with no `DEFAULT_KEYMAP` entry (a DK entry's `label`
 *  names the rest — one source for the Shortcuts view and the Actions tab).
 *  Covers every §10.2 / §10.3 canonical built-in id that no DK entry names
 *  today; ids WP-54 / WP-56 add to `defaults.ts` fall back to their DK label,
 *  or to the id itself. */
const BUILTIN_NAMES: ReadonlyArray<readonly [string, string, boolean?]> = [
	// Palette / shortcuts (WP-54 adds the DK entries)
	['palette.close', 'Close command palette'],
	['palette.toggle-shortcuts', 'Toggle shortcuts view'],
	// Terminal (hosted, WP-54)
	['terminal.copy', 'Copy (terminal)'],
	['terminal.paste', 'Paste (terminal)'],
	['terminal.find', 'Find (terminal)'],
	['terminal.select-all', 'Select all (terminal)'],
	['terminal.prev-prompt', 'Previous prompt (terminal)'],
	['terminal.next-prompt', 'Next prompt (terminal)'],
	// Pane and tab
	['pane.tab-prev', 'Previous tab'],
	['pane.tab-next', 'Next tab'],
	['pane.focus-up', 'Move pane focus up'],
	['pane.focus-down', 'Move pane focus down'],
	['pane.back', 'Back'],
	['pane.forward', 'Forward'],
	['pane.screenshot', 'Screenshot'],
	['pane.refresh', 'Refresh'],
	['tab.toggle-pin', 'Pin / unpin tab'],
	['tab.move-left', 'Move tab left'],
	['tab.move-right', 'Move tab right'],
	['tab.move-to-new-pane-right', 'Move to new pane (right)'],
	['tab.move-to-new-pane-down', 'Move to new pane (down)'],
	['tab.close-others', 'Close other tabs'],
	['tab.close-to-right', 'Close tabs to the right'],
	// Pane ⋯ — artifact branch
	['viewer.open-in-browser', 'Open in browser'],
	['viewer.toggle-source', 'Open / close source'],
	['viewer.copy-url', 'Copy viewer URL'],
	['viewer.zoom-in', 'Zoom in (viewer)'],
	['viewer.zoom-out', 'Zoom out (viewer)'],
	['viewer.zoom-reset', 'Reset zoom (viewer)'],
	['viewer.device-phone', '390 · phone'],
	['viewer.device-tablet', '768 · tablet'],
	['viewer.device-full', 'Full width'],
	['viewer.pin-to-artifacts', 'Pin to Artifacts'],
	['viewer.toggle-history', 'Version history'],
	// Pane ⋯ — pkg branch (shipped `data-action` ids)
	['pkg.keep-blocking', 'Keep blocking'],
	['pkg.reload-view', 'Reload view'],
	['pkg.view-permissions', 'View permissions'],
	['pkg.package-settings', 'Package settings'],
	['pkg.restart-sidecar', 'Restart sidecar'],
	['pkg.unpin', 'Unpin'],
	['pkg.report-violation-log', 'Report violation log'],
	// Rail menus
	['rail.pin-open', 'Open'],
	['rail.pin-move-up', 'Move up'],
	['rail.pin-move-down', 'Move down'],
	['rail.pin-move-to-section', 'Move to'],
	['rail.pin-no-section', 'No section'],
	['rail.unpin', 'Unpin', true],
	['rail.section-rename', 'Rename…'],
	['rail.section-manage', 'Manage in Settings'],
	['rail.section-delete', 'Delete section…', true],
	['ngwa.installed', 'Installed'],
	['ngwa.store', 'Store'],
	['ngwa.health', 'Health'],
	// Files view options and the HTML viewer frame
	['explorer.toggle-hidden', 'Show hidden files'],
	['files.toggle-ignored', 'Show ignored'],
	['files.open-as-artifact-grid', 'Open as Artifact Grid'],
	['viewer.add-pin', 'Add pin / comment here…'],
	['viewer.open-in-studio', 'Open in Studio'],
	['viewer.reload', 'Reload'],
	// Explorer and section frame
	['explorer.section-prev', 'Previous Explorer section'],
	['explorer.section-next', 'Next Explorer section'],
	['section.collapse-others', 'Collapse others'],
	['section.hide', 'Hide section'],
	['section.move-up', 'Move section up'],
	['section.move-down', 'Move section down'],
	// Companion
	['companion.focus-dispatch', 'Focus the dispatch input'],
	// Native-menu commands with no DK entry. The first three lost their key
	// when WP-54 applied DEC-64 / DEC-63.2 (§2.4, §10.2: unbound, their
	// `MENU_TREE` leaves stay unaccelerated) — named here so they stay
	// built-in actions.
	['menu.new-session', 'New Session'],
	['menu.new-terminal', 'New Terminal'],
	['session.switch-adapter', 'Switch Adapter (coming soon)'],
	['ikenga.check-updates', 'Check for Updates…'],
	['project.project-settings', 'Project Settings'],
	['chi.permission-inbox', 'Permission Inbox'],
	['chi.runs', 'Runs'],
	['help.docs', 'Docs'],
	['help.feedback', 'Send Feedback'],
	// Window zoom (WP-54 adds the DK entries)
	['zoom.in', 'Zoom in'],
	['zoom.out', 'Zoom out'],
	['zoom.reset', 'Reset zoom'],
	// OS-wide (§6; WP-54 adds the DK entries)
	['os.summon', 'Summon Ikenga'],
	['os.screenshot-window', 'Screenshot window'],
	['os.screenshot-pane', 'Screenshot focused pane'],
	// Bare, grandfathered (§10.3 — the closed set of 35)
	['open', 'Open'],
	['open-to-side', 'Open to the Side'],
	['open-below', 'Open Below'],
	['open-loupe', 'Open in Loupe'],
	['open-studio', 'Open in Studio (grid)'],
	['open-in-studio', 'Open in Studio'],
	['compare', 'Compare'],
	['pin-sidebar', 'Pin to Sidebar…'],
	['pin-rail', 'Pin to Rail'],
	['open-terminal-here', 'Open Terminal Here'],
	['open-terminal-side', 'Open in Terminal to the Side'],
	['open-terminal-below', 'Open in Terminal Below'],
	['hand-to-chi', 'Hand to Chi'],
	['copy-path', 'Copy Path'],
	['copy-name', 'Copy Name'],
	['copy-uri', 'Copy ikenga:// URI'],
	['reveal-files', 'Reveal in Files'],
	['reveal-file-manager', 'Reveal in File Manager'],
	['new-file', 'New File'],
	['new-folder', 'New Folder'],
	['rename', 'Rename…'],
	['delete', 'Move to Trash', true],
	['make-dispatch', 'Make dispatch target'],
	['kill-session', 'Kill session', true],
	['run-now', 'Run now'],
	['pause-resume', 'Pause / resume'],
	['open-last-log', 'Open last log'],
	['open-definition', 'Open definition file'],
	['open-in-ngwa', 'Open in Ngwa'],
	['open-detail', 'Open detail'],
	['change-scope', 'Change scope'],
	['disable', 'Disable'],
	['uninstall', 'Uninstall', true],
	['toggle-done', 'Toggle done'],
	['open-source', 'Open source'],
];

/** The §10.3 bare grandfathered set — closed, never grows. */
export const BARE_BUILTIN_IDS: ReadonlySet<string> = new Set([
	'open',
	'open-to-side',
	'open-below',
	'open-loupe',
	'open-studio',
	'open-in-studio',
	'compare',
	'pin-sidebar',
	'pin-rail',
	'open-terminal-here',
	'open-terminal-side',
	'open-terminal-below',
	'hand-to-chi',
	'copy-path',
	'copy-name',
	'copy-uri',
	'reveal-files',
	'reveal-file-manager',
	'new-file',
	'new-folder',
	'rename',
	'delete',
	'make-dispatch',
	'kill-session',
	'run-now',
	'pause-resume',
	'open-last-log',
	'open-definition',
	'open-in-ngwa',
	'open-detail',
	'change-scope',
	'disable',
	'uninstall',
	'toggle-done',
	'open-source',
]);

let builtinCache: BuiltinActionDef[] | null = null;

/**
 * Every built-in action: each `DEFAULT_KEYMAP` command (named by its
 * `label`, first entry wins) followed by the catalog above, one row per id.
 * Recomputed only once per module load — `DEFAULT_KEYMAP` is static.
 */
export function builtinActions(): BuiltinActionDef[] {
	if (builtinCache) return builtinCache;
	const out: BuiltinActionDef[] = [];
	const seen = new Set<string>();
	const danger = new Map<string, boolean>(BUILTIN_NAMES.map(([id, , d]) => [id, d === true] as const));
	for (const entry of DEFAULT_KEYMAP) {
		if (seen.has(entry.command)) continue;
		seen.add(entry.command);
		out.push(
			danger.get(entry.command)
				? { id: entry.command, name: entry.label, danger: true }
				: { id: entry.command, name: entry.label }
		);
	}
	for (const [id, name, isDanger] of BUILTIN_NAMES) {
		if (seen.has(id)) continue;
		seen.add(id);
		out.push(isDanger ? { id, name, danger: true } : { id, name });
	}
	builtinCache = out;
	return out;
}

export function isBuiltinActionId(id: string): boolean {
	return builtinActions().some((a) => a.id === id);
}

// ─── Icons (§1.2, W_UNKNOWN_ICON) ────────────────────────────────────────────

export const DEFAULT_ACTION_ICON = 'zap';
const KNOWN_ICONS: ReadonlySet<string> = new Set<string>(iconNames);

/** Is `name` a Lucide icon name exactly as the file stores it (kebab-case)? */
export function isKnownIconName(name: string): boolean {
	return KNOWN_ICONS.has(name);
}

/** The icon a user action renders: its own when known, else `zap` (§1.2). */
export function resolveActionIcon(icon: string | undefined): string {
	return icon && isKnownIconName(icon) ? icon : DEFAULT_ACTION_ICON;
}

// ─── Package contributions (kernel snapshot) ─────────────────────────────────

/** `ContextSelector` wire shape (`manifest.rs`, serde `tag = "kind"`). */
export type ContextSelector =
	| { kind: 'file'; glob?: string | null }
	| { kind: 'artifact' }
	| { kind: 'session' }
	| { kind: 'ngwa-item'; kinds?: string[] | null };

/** `ContextActionRun` wire shape — the package run union (DEC-55). */
export type PackageRun = { kind: 'dispatch'; prompt: string; target?: string | null } | { kind: 'view'; route: string };

/** `ContextActionRegistryEntry` (`pkg/registries/context_actions.rs`). */
export interface ContextActionRegistryEntry {
	pkg_id: string;
	qualified_id: string;
	id: string;
	label: string;
	when: ContextSelector;
	run: PackageRun;
	key?: string | null;
}

/** `CommandPaletteRegistryEntry` (same registry, `command_palette`, §12). */
export interface CommandPaletteRegistryEntry {
	pkg_id: string;
	qualified_id: string;
	id: string;
	label: string;
	shortcut?: string | null;
	action: PackageRun;
}

/** One package action as the merge consumes it, in **grant order** (§7.4:
 *  earliest `installed_at`, then pkg id, then declaration order — context
 *  actions before palette entries, the manifest's field order). */
export interface PackageActionSource {
	/** `${pkg_id}:${id}`. */
	id: string;
	pkgId: string;
	localId: string;
	name: string;
	origin: 'context_action' | 'command_palette';
	run: PackageRun;
	/** `context_action` only. */
	selector?: ContextSelector;
	/** The DEC-54 request (`key` / `shortcut`), verbatim; null when none. */
	keyRequest: string | null;
	installedAt: number;
}

interface KernelSnapshotLike {
	installed?: Array<{ id: string; installed_at?: number | null }>;
	registries?: Record<string, unknown>;
}

function asArray<T>(value: unknown): T[] {
	return Array.isArray(value) ? (value as T[]) : [];
}

/**
 * Reads the package actions out of a `pkg_kernel_status()` result: the
 * `context_actions` registry snapshot `{ entries, command_palette: { entries } }`
 * (WP-52 added the palette projection). Tolerates an older shell's snapshot
 * without `command_palette` (no palette entries) and a missing registry.
 */
export function readPackageActions(status: KernelSnapshotLike | null | undefined): PackageActionSource[] {
	const snapshot = (status?.registries?.context_actions ?? null) as {
		entries?: unknown;
		command_palette?: { entries?: unknown } | null;
	} | null;
	const installedAt = new Map<string, number>();
	for (const row of status?.installed ?? []) {
		if (typeof row.installed_at === 'number') installedAt.set(row.id, row.installed_at);
	}
	const at = (pkgId: string) => installedAt.get(pkgId) ?? Number.MAX_SAFE_INTEGER;

	const rows: Array<{ src: PackageActionSource; decl: number }> = [];
	let decl = 0;
	for (const e of asArray<ContextActionRegistryEntry>(snapshot?.entries)) {
		rows.push({
			decl: decl++,
			src: {
				id: e.qualified_id || `${e.pkg_id}:${e.id}`,
				pkgId: e.pkg_id,
				localId: e.id,
				name: e.label,
				origin: 'context_action',
				run: e.run,
				selector: e.when,
				keyRequest: e.key ?? null,
				installedAt: at(e.pkg_id),
			},
		});
	}
	for (const e of asArray<CommandPaletteRegistryEntry>(snapshot?.command_palette?.entries)) {
		rows.push({
			decl: decl++,
			src: {
				id: e.qualified_id || `${e.pkg_id}:${e.id}`,
				pkgId: e.pkg_id,
				localId: e.id,
				name: e.label,
				origin: 'command_palette',
				run: e.action,
				keyRequest: e.shortcut ?? null,
				installedAt: at(e.pkg_id),
			},
		});
	}
	// The snapshot lists pkgs by id, context actions before palette entries,
	// each in declaration order — so `decl` already orders within a pkg.
	rows.sort(
		(a, b) =>
			a.src.installedAt - b.src.installedAt ||
			(a.src.pkgId < b.src.pkgId ? -1 : a.src.pkgId > b.src.pkgId ? 1 : 0) ||
			a.decl - b.decl
	);
	return rows.map((row) => row.src);
}

// ─── Derivations (§7.3, §7.3a, §12) ──────────────────────────────────────────

function quoteWhenString(s: string): string {
	return `'${s.replace(/\\/g, '\\\\').replace(/'/g, "\\'")}'`;
}

/**
 * §7.3 (G-71): a `context_actions[]` key request's `when`, derived from its
 * `ContextSelector` — the TS mirror of `manifest.rs`'s
 * `derive_context_action_key_when` (same strings, byte for byte). Every row
 * is narrower than `always`. An empty glob counts as absent; `kinds: []` is
 * no filter (B-12).
 */
export function deriveContextActionKeyWhen(selector: ContextSelector): string {
	switch (selector.kind) {
		case 'file':
			return selector.glob ? `filesFocus && resource =~ ${quoteWhenString(selector.glob)}` : 'filesFocus';
		case 'artifact':
			return "paneKind == 'artifact'";
		case 'session':
			return 'sessionFocus';
		case 'ngwa-item': {
			const kinds = selector.kinds ?? [];
			if (kinds.length === 0) return 'ngwaItemFocus';
			return `ngwaItemFocus && (${kinds.map((k) => `ngwaItemKind == ${quoteWhenString(k)}`).join(' || ')})`;
		}
	}
}

/** §12: a `command_palette[].shortcut` request's derived `when`. */
export const PALETTE_SHORTCUT_WHEN = '!inputFocus';

/** A package action's key request `when` (§7.3 or §12). */
export function packageKeyWhen(action: PackageActionSource): string {
	return action.origin === 'command_palette' || !action.selector
		? PALETTE_SHORTCUT_WHEN
		: deriveContextActionKeyWhen(action.selector);
}

/** Applicability condition a renderer evaluates on the object a menu opened
 *  on (§1.3's italic annotations); an item whose condition is false is
 *  skipped, not disabled. */
export type MenuItemCondition =
	| 'file'
	| 'dir'
	| 'history'
	| 'pkg-pane'
	| 'pkg-blocking'
	| 'pkg-supervised'
	| 'pkg-pinned'
	| 'artifact-tab'
	| 'artifact-or-route-tab'
	| 'html-in-pane'
	| 'sectioned-pin';

/** A placement as the merge reads it: a file `Placement`, or a derived
 *  package placement that may carry an applicability `condition`. */
export interface ResolvedPlacement {
	at: string;
	/** DEC-62 `when`, evaluated in the menu context (§1.3). */
	when?: string;
	condition?: MenuItemCondition;
}

/**
 * §7.3a: where a package action appears. A `context_actions[]` entry's menus
 * follow from its selector (the focus atom dropped, the rest evaluated in the
 * menu context); a `command_palette[]` entry is placed in `palette` (§12).
 */
export function packagePlacements(action: PackageActionSource): ResolvedPlacement[] {
	if (action.origin === 'command_palette' || !action.selector) return [{ at: 'palette' }];
	const selector = action.selector;
	switch (selector.kind) {
		case 'file':
			return selector.glob
				? [{ at: 'files', when: `resource =~ ${quoteWhenString(selector.glob)}`, condition: 'file' }]
				: [{ at: 'files', condition: 'file' }];
		case 'artifact':
			return [{ at: 'artifacts' }, { at: 'pane', condition: 'artifact-tab' }];
		case 'session':
			return [{ at: 'session' }];
		case 'ngwa-item': {
			const kinds = selector.kinds ?? [];
			return kinds.length === 0
				? [{ at: 'ngwa-project' }]
				: [
						{
							at: 'ngwa-project',
							when: kinds.map((k) => `ngwaItemKind == ${quoteWhenString(k)}`).join(' || '),
						},
					];
		}
	}
}

/** A user action's placements (§1.3) in the merge's shape. */
export function userPlacements(action: UserAction): ResolvedPlacement[] {
	return (action.placements ?? [])
		.filter((p): p is Placement => Boolean(p) && typeof p.at === 'string')
		.map((p) => (typeof p.when === 'string' && p.when ? { at: p.at, when: p.when } : { at: p.at }));
}

// ─── Effective actions ───────────────────────────────────────────────────────

/** Where an effective action is defined (§0's four sources). */
export type ActionSource = 'builtin' | 'package' | 'personal' | 'project';

/** A built-in's run: code, never serialized (§8.1). */
export interface BuiltinRun {
	kind: 'builtin';
}

export type EffectiveRun = BuiltinRun | PackageRun | ActionRun;

/** One action in the effective model. */
export interface EffectiveAction {
	id: string;
	name: string;
	/** Lucide name: a user action's own when known (else `zap`); a package
	 *  action's `zap`; undefined for built-ins (their surfaces own glyphs). */
	icon?: string;
	description: string;
	source: ActionSource;
	run: EffectiveRun;
	/** Menus this action is placed in by its source (user / package). A
	 *  built-in's default membership lives in the menu defaults instead. */
	placements: ResolvedPlacement[];
	/** Never-hide (§9.2). */
	locked: boolean;
	danger: boolean;
	/** Fired by its owner widget, never the frame dispatcher (§4.6). */
	hosted: boolean;
	/** `os.*`: bindable only with `scope: "os"` (§6). */
	osOnly: boolean;
	/** Built-ins may be hidden and rebound, never edited (§9.1). */
	editable: boolean;
	/** Package actions: the owning pkg, which list, the selector, and the key
	 *  request (verbatim). */
	pkgId?: string;
	packageOrigin?: 'context_action' | 'command_palette';
	selector?: ContextSelector;
	keyRequest?: string | null;
	/** User actions: the file definition (unmodified) and its index. */
	userAction?: UserAction;
	fileIndex?: number;
	/** A personal action whose id the project redefines (§1.2). Only set on
	 *  entries in `EffectiveModel.shadowedActions`. */
	overriddenBy?: 'project';
}

export function builtinToEffective(def: BuiltinActionDef): EffectiveAction {
	return {
		id: def.id,
		name: def.name,
		description: '',
		source: 'builtin',
		run: { kind: 'builtin' },
		placements: [],
		locked: isLockedAction(def.id),
		danger: def.danger === true,
		hosted: isHostedCommand(def.id),
		osOnly: isOsOnlyAction(def.id),
		editable: false,
	};
}

export function packageToEffective(src: PackageActionSource): EffectiveAction {
	return {
		id: src.id,
		name: src.name,
		icon: DEFAULT_ACTION_ICON,
		description: '',
		source: 'package',
		run: src.run,
		placements: packagePlacements(src),
		locked: false,
		danger: false,
		hosted: false,
		osOnly: false,
		editable: false,
		pkgId: src.pkgId,
		packageOrigin: src.origin,
		...(src.selector ? { selector: src.selector } : {}),
		keyRequest: src.keyRequest,
	};
}

export function userToEffective(action: UserAction, scope: 'personal' | 'project', index: number): EffectiveAction {
	return {
		id: action.id,
		name: action.name,
		icon: resolveActionIcon(action.icon),
		description: action.description ?? '',
		source: scope,
		run: action.run,
		placements: userPlacements(action),
		locked: false,
		danger: false,
		hosted: false,
		osOnly: false,
		editable: true,
		userAction: action,
		fileIndex: index,
	};
}
