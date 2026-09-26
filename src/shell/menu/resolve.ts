// WP-55 — turns `getEffectiveMenu(menuId)` (G-ACTIONS-API, WP-52) into rows a
// renderer can map straight to its own menu primitives. The merge
// (`@/lib/actions/menus.ts`) already applied layer order, package/user
// appends, `hidden` and locked-hide rejection; what's left for a renderer is
// per-open state the model can't know:
//
//   - `target` (§1.3 menu context): the row / tab / pane the menu opened on.
//     A placement `when` is evaluated against it (`buildMenuContext`), never
//     against live DOM focus, and a user action run from the menu gets its
//     `file.path` / `file.name` from it (§8.2), not from the live pane.
//   - `condition` (§1.3 italics — "(file)", "(dir)", "(history)", …): true
//     only for *this* open, so it's a caller-supplied flag, not model state.
//   - `disabled` (grayed out, still visible, with its reason as the tooltip)
//     — a boundary state ("Move left" on the first tab) the model has no
//     shape for.
//   - presentation the shipped JSX had and the model doesn't carry: a
//     dynamic label ("Reset zoom (110%)"), an icon, the `destructive` style,
//     the `data-action` attribute tests and e2e select on.
//   - dispatch: a local handler for row-specific behaviour (which file, which
//     tab) a generic runner can't reach; else `runMenuAction` — the WP-53
//     runner for a personal / project action (with the target's variables,
//     its outcome surfaced), a package action's own run, or the WP-54
//     command table for a built-in.
//
// Pure data apart from `runMenuAction` — no JSX. Every menu-owning file
// renders its own `ContextMenuItem` / `DropdownMenuItem` (they aren't
// interchangeable Radix primitives); the plain ones share
// `effective-context-menu.tsx`. Callers resolve inside a component that
// mounts only while the menu is open (Radix mounts menu content lazily), so
// nothing here runs per row while menus are closed.

import type { ReactNode } from 'react';
import { collapseSeparators, type EffectiveMenu, getEffectiveAction, type MenuItemCondition } from '@/lib/actions/store';
import { runCommand } from '@/lib/keymap/commands';
import { buildMenuContext, getEvalOptions } from '@/lib/keymap/context-keys';
import { labelFor } from '@/lib/keymap/registry';
import { type EvalOptions, evaluateWhen } from '@/lib/keymap/when';
import type { PaneView } from '@/lib/panes/types';
import { useShellStore } from '@/lib/shell/shell-store';

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
	/** The action's own `danger` flag, or the caller's `destructive` list —
	 *  renders with the destructive style. */
	danger: boolean;
	locked: boolean;
	disabled: boolean;
	/** Why the item is disabled — the item's tooltip (`title`). */
	disabledReason?: string;
	display?: 'checkbox' | 'radio' | 'submenu';
	group?: string;
	icon?: ReactNode;
	/** Value of the item's `data-action` attribute. */
	dataAction: string;
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
	handlers?: Readonly<Record<string, () => void>>;
	/** Per-id disabled (grayed out, still visible). A string is the reason,
	 *  shown as the item's tooltip; `true` disables with no tooltip. */
	disabled?: (id: string) => boolean | string | undefined;
	/** Per-id label override (a dynamic label: "Close source", "Reset zoom
	 *  (110%)"). The effective name is used otherwise. */
	labels?: Readonly<Record<string, string | undefined>>;
	icons?: Readonly<Record<string, ReactNode>>;
	/** Ids that render with the destructive style although the action isn't
	 *  flagged `danger` ("Close pane"). */
	destructive?: readonly string[];
	/** `data-action` values that differ from the id (shipped attributes). */
	dataActions?: Readonly<Record<string, string>>;
	/** A-9: a built-in with no local handler here has no behaviour on this
	 *  surface — skip it instead of rendering a no-op. User and package items
	 *  still render (they run through `runMenuAction`). */
	builtinsNeedHandler?: boolean;
}

function conditionHolds(condition: MenuItemCondition | undefined, flags: Partial<Record<MenuItemCondition, boolean>>): boolean {
	return condition === undefined || flags[condition] === true;
}

/** Eval options for a placement `when`, read defensively: a partial shell-store
 *  mock (or no active project) must not throw during a render. */
function safeEvalOptions(): EvalOptions {
	try {
		return getEvalOptions() ?? {};
	} catch {
		return {};
	}
}

/** The active project for the menu context (§4.3: set only when it has a root). */
function liveProjectKey(): string | undefined {
	try {
		const active = useShellStore.getState()?.activeProject ?? null;
		return active?.root_path ? active.id : undefined;
	} catch {
		return undefined;
	}
}

function basenameOf(path: string): string {
	const norm = path.replace(/\\/g, '/').replace(/\/+$/, '');
	return norm.slice(norm.lastIndexOf('/') + 1);
}

/** §8.2 variables the menu target supplies (the row, not the live pane). */
export function menuVariables(target: MenuTarget | undefined): { 'file.path'?: string; 'file.name'?: string } {
	if (!target?.resource) return {};
	return { 'file.path': target.resource, 'file.name': basenameOf(target.resource) };
}

/**
 * Runs a menu item that has no local handler:
 *
 *   - a personal / project action → the WP-53 runner, with `file.path` /
 *     `file.name` from the menu target; its `RunOutcome` is awaited and a
 *     refusal or failure is surfaced (notice + trust sheet, `run-notice.tsx`);
 *   - a package action → its own fill-only `dispatch` / `view` run, with the
 *     target's variables interpolated;
 *   - anything else (a built-in) → the WP-54 command table, which reaches
 *     the command's registered owner.
 */
export function runMenuAction(id: string, target?: MenuTarget): void {
	let action: ReturnType<typeof getEffectiveAction>;
	try {
		action = getEffectiveAction(id);
	} catch {
		action = undefined;
	}
	if (action && (action.source === 'personal' || action.source === 'project') && action.userAction) {
		const userAction = action.userAction;
		const scope = action.source;
		const name = action.name;
		void (async () => {
			const [{ runAction }, { surfaceRunOutcome, confirmShellRun }] = await Promise.all([
				import('@/lib/actions/runner'),
				import('./run-notice'),
			]);
			const outcome = await runAction(
				{ id, name, run: userAction.run, scope },
				{ variables: menuVariables(target), confirm: confirmShellRun }
			);
			surfaceRunOutcome(outcome);
		})().catch((err) => console.warn(`[menu] could not run action "${id}":`, err));
		return;
	}
	if (action && action.source === 'package') {
		const run = action.run as { kind?: string; prompt?: string; route?: string };
		if (run.kind === 'dispatch' || run.kind === 'view') {
			void (async () => {
				const { gatherRunVariables, interpolate } = await import('@/lib/actions/runner');
				const template = run.kind === 'dispatch' ? (run.prompt ?? '') : (run.route ?? '');
				const vars = await gatherRunVariables({ kind: 'chi', target: 'active', prompt: template }, menuVariables(target));
				let text = template;
				try {
					text = interpolate(template, vars, run.kind === 'dispatch' ? 'raw' : 'uri');
				} catch {
					// An unknown variable in package content: run it verbatim.
				}
				if (run.kind === 'dispatch') {
					// Fill-only (DEC-63.3): pre-fill the dispatch input, never send.
					const { handToChi } = await import('@/shell/companion/companion-store');
					handToChi(text);
				} else {
					const { usePaneStore } = await import('@/lib/panes/pane-store');
					usePaneStore.getState().navigateFocused(text);
				}
			})().catch((err) => console.warn(`[menu] could not run action "${id}":`, err));
			return;
		}
	}
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
	const target = opts.target;
	// Built lazily: most menus have no placement `when` at all.
	let ctx: ReturnType<typeof buildMenuContext> | null = null;
	let evalOpts: EvalOptions | null = null;
	const rows: ResolvedMenuRow[] = [];
	for (const entry of menu.items) {
		if (entry.kind === 'separator') {
			rows.push({ kind: 'separator' });
			continue;
		}
		if (!conditionHolds(entry.condition, flags)) continue;
		if (entry.when) {
			ctx ??= buildMenuContext({ ...target, project: target?.project ?? liveProjectKey() });
			evalOpts ??= safeEvalOptions();
			if (!evaluateWhen(entry.when, ctx, evalOpts)) continue;
		}
		const handler = opts.handlers?.[entry.id];
		if (!handler && opts.builtinsNeedHandler && entry.action.source === 'builtin') continue;
		const disabled = opts.disabled?.(entry.id);
		rows.push({
			kind: 'item',
			id: entry.id,
			label: opts.labels?.[entry.id] ?? (entry.action.name || entry.id),
			danger: entry.action.danger || (opts.destructive?.includes(entry.id) ?? false),
			locked: entry.locked,
			disabled: Boolean(disabled),
			disabledReason: typeof disabled === 'string' && disabled ? disabled : undefined,
			display: entry.display,
			group: entry.group,
			icon: opts.icons?.[entry.id],
			dataAction: opts.dataActions?.[entry.id] ?? entry.id,
			shortcut: labelFor(entry.id),
			run: handler ?? (() => runMenuAction(entry.id, target)),
		});
	}
	return collapseSeparators(rows);
}
