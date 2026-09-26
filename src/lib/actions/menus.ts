// Effective menus (WP-52, G-ACTIONS §1.3, §1.4, §7.3a, §9, DEC-58, DEC-65).
//
// A menu is built in four layers: default contents (§1.3) → package
// appends (§7.3a, §12, in grant order) → personal override → project
// override (§1.4). User actions placed in a menu by their own `placements`
// join at their layer, before that layer's override. Project overrides apply
// before trust (DEC-65) — they cannot do anything new.
//
// Rules implemented here:
// - `items`, when present, is the order: ids the layer below lacks are
//   additions; ids the layer below has but `items` omits are appended at the
//   end in their lower-layer order (a built-in added by a later release still
//   appears); a lower layer's separators are replaced by the override's.
// - `hidden` accumulates across layers and removes an id from **that menu**
//   only. Hiding is not unbinding (DEC-58): nothing here reads or writes a
//   key.
// - A locked id (§9.2) in `hidden` is rejected (`E_LOCKED_HIDDEN`) — the
//   item stays — independently of the validator and the UI.
// - An unknown id in `items` / `hidden` is kept in the file, warned
//   (`W_UNKNOWN_COMMAND`), never rendered.
// - Separators: leading, trailing and doubled ones collapse
//   (`collapseSeparators`); renderers collapse again after skipping items
//   whose `condition` is false.
//
// `native/<top>` menus list their action leaves only: the predefined OS
// roles (§9.3) are not actions, have no id, and are interleaved by the
// renderer (WP-55) from `MENU_TREE`.

import type { ActionsScope, MenuOverride } from './types';
import type { EffectiveAction, MenuItemCondition } from './registry';

export const SEPARATOR = '---';

// ─── Default contents (§1.3) ─────────────────────────────────────────────────

export interface DefaultMenuItem {
	id: string;
	condition?: MenuItemCondition;
	/** Non-plain rendering (§1.3 rendering rule). */
	display?: 'checkbox' | 'radio' | 'submenu';
	/** Radio items with the same group render as one group. */
	group?: string;
}

export type DefaultMenuEntry = DefaultMenuItem | typeof SEPARATOR;

const S = SEPARATOR;
const c = (id: string, condition: MenuItemCondition): DefaultMenuItem => ({ id, condition });
const i = (id: string): DefaultMenuItem => ({ id });
const radio = (id: string, condition: MenuItemCondition): DefaultMenuItem => ({
	id,
	condition,
	display: 'radio',
	group: 'device-width',
});

/** The fixed menu ids of §1.3 (the parameterized `section/<id>` and
 *  `native/<top>` are handled by `defaultMenuContents`). */
export const DEFAULT_MENUS: Readonly<Record<string, readonly DefaultMenuEntry[]>> = {
	files: [
		c('open', 'file'),
		c('open-to-side', 'file'),
		c('open-below', 'file'),
		S,
		c('pin-sidebar', 'file'),
		S,
		c('files.open-as-artifact-grid', 'dir'),
		c('open-terminal-here', 'dir'),
		c('open-terminal-side', 'dir'),
		c('open-terminal-below', 'dir'),
		S,
		i('hand-to-chi'),
		i('copy-path'),
		i('copy-name'),
		S,
		i('rename'),
		i('delete'),
	],
	'files-view': [
		{ id: 'explorer.toggle-hidden', display: 'checkbox' },
		{ id: 'files.toggle-ignored', display: 'checkbox' },
	],
	artifacts: [
		i('open-loupe'),
		i('open-studio'),
		i('compare'),
		i('open-to-side'),
		S,
		i('pin-sidebar'),
		i('copy-uri'),
		i('reveal-files'),
		S,
		i('hand-to-chi'),
	],
	session: [i('open'), i('open-to-side'), i('make-dispatch'), S, i('hand-to-chi'), S, i('rename'), i('kill-session')],
	automations: [i('run-now'), i('pause-resume'), i('open-definition'), i('open-last-log'), i('open-in-ngwa')],
	'ngwa-project': [i('open-detail'), i('open-definition'), i('change-scope'), i('disable'), i('uninstall')],
	scratchpads: [i('open'), i('open-to-side'), i('rename'), i('delete')],
	todos: [i('toggle-done'), i('open-source'), i('hand-to-chi')],
	views: [i('open'), i('open-to-side'), i('pin-rail'), i('open-in-ngwa')],
	tab: [
		c('pin-sidebar', 'artifact-tab'),
		i('tab.toggle-pin'),
		S,
		i('tab.move-left'),
		i('tab.move-right'),
		S,
		i('tab.move-to-new-pane-right'),
		i('tab.move-to-new-pane-down'),
		S,
		i('copy-path'),
		S,
		i('tab.close'),
		i('tab.close-others'),
		i('tab.close-to-right'),
	],
	address: [
		i('pin-sidebar'),
		i('tab.toggle-pin'),
		S,
		c('copy-path', 'artifact-or-route-tab'),
		S,
		i('tab.close'),
	],
	pane: [
		c('pane.back', 'history'),
		c('pane.forward', 'history'),
		S,
		// pkg branch (pkg pane)
		c('pkg.keep-blocking', 'pkg-blocking'),
		c('pkg.reload-view', 'pkg-pane'),
		c('pkg.view-permissions', 'pkg-pane'),
		c('pkg.package-settings', 'pkg-pane'),
		c('pkg.restart-sidecar', 'pkg-supervised'),
		S,
		c('pkg.unpin', 'pkg-pinned'),
		c('pkg.report-violation-log', 'pkg-pane'),
		S,
		i('pane.split-right'),
		i('pane.split-down'),
		S,
		c('copy-path', 'artifact-or-route-tab'),
		// artifact branch (artifact tab)
		S,
		c('viewer.open-in-browser', 'artifact-tab'),
		c('viewer.toggle-source', 'artifact-tab'),
		c('reveal-files', 'artifact-tab'),
		c('viewer.copy-url', 'artifact-tab'),
		S,
		c('viewer.zoom-in', 'artifact-tab'),
		c('viewer.zoom-out', 'artifact-tab'),
		c('viewer.zoom-reset', 'artifact-tab'),
		S,
		radio('viewer.device-phone', 'artifact-tab'),
		radio('viewer.device-tablet', 'artifact-tab'),
		radio('viewer.device-full', 'artifact-tab'),
		S,
		c('pane.screenshot', 'artifact-tab'),
		c('viewer.pin-to-artifacts', 'artifact-tab'),
		c('hand-to-chi', 'artifact-tab'),
		c('viewer.toggle-history', 'artifact-tab'),
		S,
		i('pane.close'),
	],
	'viewer-frame': [i('viewer.add-pin'), i('copy-path'), c('viewer.open-in-studio', 'html-in-pane'), i('viewer.reload')],
	// No shipped status-bar menu items on 5b; WP-55 owns the defaults (§15 A-7).
	status: [],
	rail: [
		i('rail.pin-open'),
		S,
		i('rail.pin-move-up'),
		i('rail.pin-move-down'),
		S,
		{ id: 'rail.pin-move-to-section', display: 'submenu' },
		c('rail.pin-no-section', 'sectioned-pin'),
		S,
		i('rail.unpin'),
	],
	'rail-section': [i('rail.section-rename'), i('rail.section-manage'), S, i('rail.section-delete')],
	'rail-ngwa': [i('ngwa.installed'), i('ngwa.store'), i('ngwa.health')],
	palette: [
		i('pane.split-right'),
		i('pane.split-down'),
		i('pane.reopen'),
		S,
		i('explorer.toggle'),
		i('companion.toggle'),
		i('palette.projects'),
		i('shortcuts.open'),
	],
	// `MENU_TREE` action leaves, shipped order (§10.4); OS roles excluded.
	'native/ikenga': [i('ikenga.check-updates'), i('rail.settings')],
	'native/file': [i('menu.new-session'), i('menu.open-file'), i('menu.open-project-folder'), i('tab.close')],
	'native/edit': [],
	'native/view': [
		i('explorer.toggle'),
		i('palette.open'),
		i('companion.toggle'),
		i('pane.split-right'),
		i('pane.split-down'),
	],
	'native/project': [i('palette.projects'), i('project.project-settings')],
	'native/chi': [i('menu.new-terminal'), i('session.switch-adapter'), i('chi.permission-inbox'), i('chi.runs')],
	'native/ngwa': [i('ngwa.installed'), i('ngwa.store'), i('ngwa.health'), i('ngwa.create')],
	'native/window': [],
	'native/help': [i('help.docs'), i('help.feedback'), i('shortcuts.open')],
};

/** Every section menu's default contents (`section/<sectionId>`). */
export const SECTION_MENU_DEFAULTS: readonly DefaultMenuEntry[] = [
	i('section.collapse-others'),
	i('section.hide'),
	S,
	i('section.move-up'),
	i('section.move-down'),
];

/** The fixed menu ids (§1.3) — `section/<id>` menus are open-ended. */
export const FIXED_MENU_IDS: readonly string[] = Object.keys(DEFAULT_MENUS);

/** Is `menuId` a menu the shell renders (§1.3)? */
export function isKnownMenuId(menuId: string): boolean {
	if (menuId in DEFAULT_MENUS) return true;
	const section = menuId.startsWith('section/') ? menuId.slice('section/'.length) : null;
	return section !== null && section.length > 0 && !section.includes('/');
}

/** Default contents of `menuId`, or null for an unknown menu id. */
export function defaultMenuContents(menuId: string): readonly DefaultMenuEntry[] | null {
	const fixed = DEFAULT_MENUS[menuId];
	if (fixed) return fixed;
	return isKnownMenuId(menuId) ? SECTION_MENU_DEFAULTS : null;
}

// ─── Effective menus ─────────────────────────────────────────────────────────

/** The layer an item entered the menu at. */
export type MenuLayer = 'default' | 'package' | 'personal' | 'project';

export interface EffectiveMenuActionItem {
	kind: 'action';
	id: string;
	action: EffectiveAction;
	layer: MenuLayer;
	/** Placement `when` (§1.3), evaluated by the renderer in the menu
	 *  context (`buildMenuContext`); absent = shown. */
	when?: string;
	/** Applicability condition (§1.3 annotations) — false = skipped. */
	condition?: MenuItemCondition;
	display?: DefaultMenuItem['display'];
	group?: string;
	/** §9.2: can be moved, never hidden. */
	locked: boolean;
}

export interface EffectiveMenuSeparator {
	kind: 'separator';
}

export type EffectiveMenuItem = EffectiveMenuActionItem | EffectiveMenuSeparator;

export interface EffectiveMenu {
	id: string;
	/** Rendered items, in order, hidden ids removed, separators collapsed. */
	items: EffectiveMenuItem[];
	/** Ids hidden from this menu (accumulated personal + project) that the
	 *  menu would otherwise contain — the Menus tab's "hidden" list. */
	hidden: string[];
	/** The user overrides in force for this menu, per scope. */
	overrides: Partial<Record<ActionsScope, MenuOverride>>;
}

/** A merge-time problem (reported with the file issues). */
export interface MenuMergeIssue {
	code: 'E_LOCKED_HIDDEN' | 'W_UNKNOWN_COMMAND';
	scope: ActionsScope;
	menuId: string;
	/** `items` or `hidden`. */
	field: 'items' | 'hidden';
	index: number;
	id: string;
	message: string;
}

export interface MenuMergeInput {
	menuId: string;
	/** Effective actions by id (every source; project user actions win). */
	actions: ReadonlyMap<string, EffectiveAction>;
	/** Package actions in grant order (§7.4). */
	packageActions: readonly EffectiveAction[];
	/** Winning user actions by scope, in file order. */
	userActions: Readonly<Record<ActionsScope, readonly EffectiveAction[]>>;
	/** In-force `menus` of each file (DEC-65: project applies before trust). */
	overrides: Partial<Record<ActionsScope, Record<string, MenuOverride> | undefined>>;
}

type WorkItem = EffectiveMenuActionItem | EffectiveMenuSeparator;

function placementsAt(action: EffectiveAction, menuId: string, layer: MenuLayer): EffectiveMenuActionItem[] {
	return action.placements
		.filter((p) => p.at === menuId)
		.slice(0, 1)
		.map((p) => ({
			kind: 'action' as const,
			id: action.id,
			action,
			layer,
			...(p.when ? { when: p.when } : {}),
			...(p.condition ? { condition: p.condition } : {}),
			locked: action.locked,
		}));
}

/** Where package appends go: the end of the menu, except `pane`, where
 *  they close the artifact branch — before the trailing separator and
 *  `pane.close` (§7.3a). */
function packageInsertIndex(menuId: string, work: readonly WorkItem[]): number {
	if (menuId !== 'pane') return work.length;
	const close = work.findIndex((item) => item.kind === 'action' && item.id === 'pane.close');
	if (close < 0) return work.length;
	return close > 0 && work[close - 1].kind === 'separator' ? close - 1 : close;
}

function containsId(items: readonly WorkItem[], id: string): boolean {
	return items.some((item) => item.kind === 'action' && item.id === id);
}

function applyOverride(
	lower: WorkItem[],
	override: MenuOverride,
	scope: ActionsScope,
	input: MenuMergeInput,
	issues: MenuMergeIssue[]
): WorkItem[] {
	const items = Array.isArray(override.items) ? override.items : null;
	if (!items) return lower;
	const byId = new Map<string, EffectiveMenuActionItem>();
	for (const item of lower) if (item.kind === 'action' && !byId.has(item.id)) byId.set(item.id, item);

	const out: WorkItem[] = [];
	const placed = new Set<string>();
	items.forEach((id, index) => {
		if (typeof id !== 'string') return;
		if (id === SEPARATOR) {
			out.push({ kind: 'separator' });
			return;
		}
		if (placed.has(id)) return;
		const existing = byId.get(id);
		if (existing) {
			out.push(existing);
			placed.add(id);
			return;
		}
		const action = input.actions.get(id);
		if (!action) {
			issues.push({
				code: 'W_UNKNOWN_COMMAND',
				scope,
				menuId: input.menuId,
				field: 'items',
				index,
				id,
				message: `no action defines \`${id}\` (kept in the file, not rendered)`,
			});
			return;
		}
		// An addition (§1.4). A placement on the action for this menu keeps
		// its `when` / `condition`.
		const placement = action.placements.find((p) => p.at === input.menuId);
		out.push({
			kind: 'action',
			id,
			action,
			layer: scope,
			...(placement?.when ? { when: placement.when } : {}),
			...(placement?.condition ? { condition: placement.condition } : {}),
			locked: action.locked,
		});
		placed.add(id);
	});
	// Lower-layer ids the override omits: appended in lower-layer order.
	for (const item of lower) {
		if (item.kind === 'action' && !placed.has(item.id)) {
			out.push(item);
			placed.add(item.id);
		}
	}
	return out;
}

/** Drops leading, trailing and doubled separators. */
export function collapseSeparators<T extends { kind: string }>(items: readonly T[]): T[] {
	const out: T[] = [];
	for (const item of items) {
		if (item.kind === 'separator') {
			if (out.length === 0 || out[out.length - 1].kind === 'separator') continue;
		}
		out.push(item);
	}
	while (out.length > 0 && out[out.length - 1].kind === 'separator') out.pop();
	return out;
}

/**
 * Builds one effective menu. Returns null for an unknown menu id with no
 * contents from any layer. Issues (locked-hide rejections, unknown ids) are
 * appended to `issues`.
 */
export function buildMenu(input: MenuMergeInput, issues: MenuMergeIssue[]): EffectiveMenu | null {
	const { menuId } = input;
	const defaults = defaultMenuContents(menuId);
	const personalOverride = input.overrides.personal?.[menuId];
	const projectOverride = input.overrides.project?.[menuId];

	// 1. Default contents — built-ins resolved against the model.
	let work: WorkItem[] = [];
	for (const entry of defaults ?? []) {
		if (entry === SEPARATOR) {
			work.push({ kind: 'separator' });
			continue;
		}
		const action = input.actions.get(entry.id);
		if (!action) continue;
		work.push({
			kind: 'action',
			id: entry.id,
			action,
			layer: 'default',
			...(entry.condition ? { condition: entry.condition } : {}),
			...(entry.display ? { display: entry.display } : {}),
			...(entry.group ? { group: entry.group } : {}),
			locked: action.locked,
		});
	}

	// 2. Package appends, grant order (§7.3a, §12). In `pane` they join the
	//    artifact branch: before its closing separator + `pane.close`.
	const appends: WorkItem[] = [];
	for (const action of input.packageActions) {
		if (containsId(work, action.id) || containsId(appends, action.id)) continue;
		appends.push(...placementsAt(action, menuId, 'package'));
	}
	if (appends.length > 0) work.splice(packageInsertIndex(menuId, work), 0, ...appends);

	// 3–4. Personal, then project: own placements, then the override.
	const hidden = new Set<string>();
	for (const scope of ['personal', 'project'] as const) {
		for (const action of input.userActions[scope]) {
			if (containsId(work, action.id)) continue;
			work.push(...placementsAt(action, menuId, scope));
		}
		const override = scope === 'personal' ? personalOverride : projectOverride;
		if (!override) continue;
		work = applyOverride(work, override, scope, input, issues);
		const hiddenIds = Array.isArray(override.hidden) ? override.hidden : [];
		hiddenIds.forEach((id, index) => {
			if (typeof id !== 'string') return;
			const action = input.actions.get(id);
			if (action?.locked) {
				issues.push({
					code: 'E_LOCKED_HIDDEN',
					scope,
					menuId,
					field: 'hidden',
					index,
					id,
					message: `\`${id}\` is locked: it can be reordered, never hidden (rejected by the merge)`,
				});
				return;
			}
			if (!action) {
				issues.push({
					code: 'W_UNKNOWN_COMMAND',
					scope,
					menuId,
					field: 'hidden',
					index,
					id,
					message: `no action defines \`${id}\` (kept in the file, inert)`,
				});
			}
			hidden.add(id);
		});
	}

	if (!defaults && work.length === 0 && !personalOverride && !projectOverride) return null;

	const hiddenPresent = new Set<string>();
	const visible = work.filter((item) => {
		if (item.kind !== 'action' || !hidden.has(item.id)) return true;
		hiddenPresent.add(item.id);
		return false;
	});
	const overrides: Partial<Record<ActionsScope, MenuOverride>> = {};
	if (personalOverride) overrides.personal = personalOverride;
	if (projectOverride) overrides.project = projectOverride;
	return {
		id: menuId,
		items: collapseSeparators(visible),
		hidden: [...hiddenPresent],
		overrides,
	};
}

/** Menu ids to materialize: the fixed ones plus every `section/<id>` (and
 *  any other known id) a placement or an override names. */
export function menuIdsFor(input: Omit<MenuMergeInput, 'menuId'>): string[] {
	const ids = new Set<string>(FIXED_MENU_IDS);
	const note = (id: string) => {
		if (isKnownMenuId(id)) ids.add(id);
	};
	for (const action of input.packageActions) for (const p of action.placements) note(p.at);
	for (const scope of ['personal', 'project'] as const) {
		for (const action of input.userActions[scope]) for (const p of action.placements) note(p.at);
		for (const id of Object.keys(input.overrides[scope] ?? {})) note(id);
	}
	return [...ids];
}
