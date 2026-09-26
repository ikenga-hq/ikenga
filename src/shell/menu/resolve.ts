// WP-55 — turns `getEffectiveMenu(menuId)` (G-ACTIONS-API, WP-52) into rows a
// renderer can map straight to its own menu primitives. The merge
// (`@/lib/actions/menus.ts`) already applied layer order, package/user
// appends, `hidden` and locked-hide rejection; what's left for a renderer is
// per-open state the model can't know:
//
//   - `condition` (§1.3 italics — "(file)", "(dir)", "(history)", …): true
//     only for *this* row/tab/pane the menu opened on, so it's a caller-
//     supplied flag, not model state.
//   - the placement `when` (§1.3) — evaluated in the **menu context**
//     (`buildMenuContext`), bound to the object the menu opened on, never to
//     live DOM focus.
//   - `disabled` (grayed out, still visible) — the model only has
//     shown/hidden; a boundary state ("Move left" on the first tab) is a
//     renderer concern with no G-ACTIONS shape of its own.
//   - dispatch: a local override for row-specific behaviour (which file,
//     which tab) a generic runner can't reach, else the WP-53 runner / the
//     WP-54 command table (`runCommand`) — a built-in's owner handler, a
//     personal/project action, or a package's fill-only `dispatch` / `view`.
//
// Pure data — no JSX. Every menu-owning file still renders its own
// `ContextMenuItem` / `DropdownMenuItem` (they aren't interchangeable Radix
// primitives).

import { runCommand } from '@/lib/keymap/commands';
import { buildMenuContext, getEvalOptions } from '@/lib/keymap/context-keys';
import { labelFor } from '@/lib/keymap/registry';
import { evaluateWhen } from '@/lib/keymap/when';
import { collapseSeparators, type EffectiveMenu, type MenuItemCondition } from '@/lib/actions/store';
import type { PaneView } from '@/lib/panes/types';

/** The object a menu opened on (§1.3 menu context) — omit for a menu with no
 *  such object (native, rail, palette, section header). */
export interface MenuTarget {
	resource?: string;
	paneKind?: PaneView['kind'];
	ngwaItemKind?: string;
	project?: string;
}

export interface RenderableMenuItem {
	kind: 'item';
	id: string;
	label: string;
	danger: boolean;
	locked: boolean;
	disabled: boolean;
	display?: 'checkbox' | 'radio' | 'submenu';
	group?: string;
	/** Accelerator label from the effective keymap registry (§1.3's rule:
	 *  shortcut labels never come from anywhere else), or `''`. */
	shortcut: string;
	run: () => void;
}

export interface RenderableSeparator {
	kind: 'separator';
}

export type ResolvedMenuRow = RenderableMenuItem | RenderableSeparator;

export interface ResolveMenuOptions {
	target?: MenuTarget;
	/** Applicability flags true for *this* open — an item whose `condition`
	 *  isn't here (or is false) is skipped, not disabled (§1.3 rendering rule). */
	conditions?: Partial<Record<MenuItemCondition, boolean>>;
	/** Per-id override of the default dispatch. */
	handlers?: Record<string, () => void>;
	/** Per-id disabled (grayed out, still visible) — a boundary state (first
	 *  tab, single pane) the effective model has no room for. */
	disabled?: (id: string) => boolean;
}

function conditionHolds(condition: MenuItemCondition | undefined, flags: Partial<Record<MenuItemCondition, boolean>>): boolean {
	return condition === undefined || flags[condition] === true;
}

/** The one fallback dispatch every menu item without a local handler runs
 *  through: the WP-54 command table (`commands.ts`) — a registered owner
 *  handler, else the WP-53 runner / a package's fill-only `dispatch` / `view`
 *  for a personal, project or package action id. */
export function runMenuAction(id: string): void {
	runCommand({ command: id, source: 'menu' });
}

/**
 * Turns one effective menu into rows ready to render. Returns `[]` for a
 * null / contentless menu. Leading, trailing and doubled separators collapse
 * again after `condition` / `when` skip which items the merge couldn't know
 * about (§1.3 rendering rule).
 */
export function resolveMenuItems(
	menu: EffectiveMenu | null | undefined,
	opts: ResolveMenuOptions = {}
): ResolvedMenuRow[] {
	if (!menu) return [];
	const flags = opts.conditions ?? {};
	const ctx = buildMenuContext(opts.target ?? {});
	const evalOpts = getEvalOptions();
	const rows: ResolvedMenuRow[] = [];
	for (const entry of menu.items) {
		if (entry.kind === 'separator') {
			rows.push({ kind: 'separator' });
			continue;
		}
		if (!conditionHolds(entry.condition, flags)) continue;
		if (entry.when && !evaluateWhen(entry.when, ctx, evalOpts)) continue;
		rows.push({
			kind: 'item',
			id: entry.id,
			label: entry.action.name || entry.id,
			danger: entry.action.danger,
			locked: entry.locked,
			disabled: opts.disabled?.(entry.id) ?? false,
			display: entry.display,
			group: entry.group,
			shortcut: labelFor(entry.id),
			run: opts.handlers?.[entry.id] ?? (() => runMenuAction(entry.id)),
		});
	}
	return collapseSeparators(rows);
}
