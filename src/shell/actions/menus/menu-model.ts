// D-06 Menus tab (WP-59): pure row-building and write-payload helpers. Split
// out from the components so the drag/keyboard/eye-toggle logic they drive
// can be unit-tested without React.
//
// Review round 1 blocker 3: a write must start from *this scope's own*
// override (`EffectiveMenu.overrides[scope]`), never blindly replace it —
// `rowsToOverridePayload` below folds the new row order back into that raw
// override rather than reconstructing one from scratch, so an unknown id
// (§1.4 — a package that isn't installed right now), or a hidden id that
// isn't part of this menu's current content at all, survives a save instead
// of being silently dropped. `menu.hidden` is the *accumulated* personal +
// project hidden set (`menus.ts`), so a row's visible "hidden" styling comes
// from that, but a *write* only ever touches this scope's own
// `overrides[scope].hidden` — never the other scope's.
//
// Review round 1 major 4: this scope's own `overrides[scope].items` (when
// present) already encodes the write-time position of every row, hidden
// ones included, because every commit below always writes the *whole*
// combined list back. `buildMenuRows` reads that position back so a hidden
// row interleaved among visible ones doesn't get regrouped to the end on
// every re-merge. A menu neither scope has ever edited from this UI has no
// such position to recover, and falls back to visible-then-hidden — the one
// documented, one-time limitation this can't remove (the merge doesn't
// expose a hidden item's pre-hide position, and G-ACTIONS-API is frozen).

import type { ActionsScope, EffectiveAction, EffectiveMenu, MenuOverride } from '@/lib/actions/store';
import { SEPARATOR } from '@/lib/actions/store';

export type MenuRow =
	| { kind: 'separator'; key: string }
	| {
			kind: 'action';
			key: string;
			id: string;
			action: EffectiveAction;
			/** Hidden by either scope — what D-06 greys out (`.off`). */
			hidden: boolean;
			/** Hidden by *this* scope's own override — what a write here can
			 *  actually clear. */
			hiddenHere: boolean;
			/** Set when the *other* scope hid it and this one didn't: an
			 *  "unhide" here would be a no-op the merge quietly swallows
			 *  (§1.4 — each layer clears its own `hidden`). */
			hiddenElsewhere: ActionsScope | null;
			locked: boolean;
	  };

export function isActionRow(row: MenuRow): row is Extract<MenuRow, { kind: 'action' }> {
	return row.kind === 'action';
}

function otherScope(scope: ActionsScope): ActionsScope {
	return scope === 'personal' ? 'project' : 'personal';
}

/** This scope's own raw `items` (§1.4) reduced to a "row key → write-time
 *  position" map. Separators carry no id, so they're matched positionally:
 *  the Nth `"---"` in the override is paired with the Nth separator row
 *  `naturalRows` produces for this same menu. Empty when this scope has
 *  never written `items` for this menu. */
function overridePositions(rawItems: unknown, naturalRows: readonly MenuRow[]): ReadonlyMap<string, number> {
	const positions = new Map<string, number>();
	if (!Array.isArray(rawItems)) return positions;
	const separatorKeys = naturalRows.filter((row) => row.kind === 'separator').map((row) => row.key);
	let sepCursor = 0;
	rawItems.forEach((entry, index) => {
		if (entry === SEPARATOR) {
			const key = separatorKeys[sepCursor];
			sepCursor += 1;
			if (key !== undefined && !positions.has(key)) positions.set(key, index);
			return;
		}
		if (typeof entry === 'string' && !positions.has(entry)) positions.set(entry, index);
	});
	return positions;
}

/** The tree's full row list, in this scope's own write-time order when one
 *  is known (Major 4), else visible items in their effective order with
 *  hidden ones appended (see module header). An id in `menu.hidden` with no
 *  resolvable action (§1.6 `W_UNKNOWN_COMMAND`) is dropped: there is
 *  nothing to render or move. */
export function buildMenuRows(
	menu: EffectiveMenu | null,
	actionById: ReadonlyMap<string, EffectiveAction>,
	scope: ActionsScope
): MenuRow[] {
	if (!menu) return [];
	const other = otherScope(scope);
	const hiddenHereSet = new Set(menu.overrides[scope]?.hidden ?? []);
	const hiddenElsewhereSet = new Set(menu.overrides[other]?.hidden ?? []);

	const natural: MenuRow[] = [];
	menu.items.forEach((item, index) => {
		if (item.kind === 'separator') {
			natural.push({ kind: 'separator', key: `sep-${index}` });
			return;
		}
		natural.push({
			kind: 'action',
			key: item.id,
			id: item.id,
			action: item.action,
			hidden: false,
			hiddenHere: false,
			hiddenElsewhere: null,
			locked: item.locked,
		});
	});
	for (const id of menu.hidden) {
		const action = actionById.get(id);
		if (!action) continue;
		const hereHidden = hiddenHereSet.has(id);
		natural.push({
			kind: 'action',
			key: id,
			id,
			action,
			hidden: true,
			hiddenHere: hereHidden,
			hiddenElsewhere: !hereHidden && hiddenElsewhereSet.has(id) ? other : null,
			locked: action.locked,
		});
	}

	const positions = overridePositions(menu.overrides[scope]?.items, natural);
	if (positions.size === 0) return natural;
	return natural
		.map((row, naturalIndex) => ({ row, naturalIndex, pos: positions.get(row.key) ?? Number.MAX_SAFE_INTEGER }))
		.sort((a, b) => a.pos - b.pos || a.naturalIndex - b.naturalIndex)
		.map((entry) => entry.row);
}

/** Moves the row at `from` to `to`; a no-op (same array) when `to` is out of
 *  range or equal to `from`, so callers can call this unconditionally. Used
 *  for the *preview* of a drag/keyboard move (validity checks, the anchor
 *  for `moveRowAfter`) — the write itself re-resolves against fresh rows. */
export function moveRow(rows: readonly MenuRow[], from: number, to: number): MenuRow[] {
	if (to < 0 || to >= rows.length || from === to || from < 0 || from >= rows.length) return rows as MenuRow[];
	const next = rows.slice();
	const [item] = next.splice(from, 1);
	next.splice(to, 0, item);
	return next;
}

/** Would a separator sitting at `index` of `rows` get silently dropped by
 *  the merge's `collapseSeparators` (§1.4: no leading, trailing or doubled
 *  separator survives)? Blocking 2. */
export function separatorWouldCollapse(rows: readonly MenuRow[], index: number): boolean {
	if (index <= 0 || index >= rows.length - 1) return true;
	return rows[index - 1]?.kind === 'separator' || rows[index + 1]?.kind === 'separator';
}

/** Moves the row keyed `key` to sit immediately after the row keyed
 *  `afterKey` (start of the list when `null`). `null` when `key` can't be
 *  found — e.g. it was removed from the menu since this move was queued
 *  (Major 5: a write always re-resolves against *current* rows, never the
 *  stale ones captured when the drag/keypress fired). */
export function moveRowAfter(rows: readonly MenuRow[], key: string, afterKey: string | null): MenuRow[] | null {
	const from = rows.findIndex((row) => row.key === key);
	if (from < 0) return null;
	const moved = rows[from];
	const rest = rows.filter((row) => row.key !== key);
	const anchor = afterKey ? rest.findIndex((row) => row.key === afterKey) : -1;
	const at = anchor >= 0 ? anchor + 1 : 0;
	return [...rest.slice(0, at), moved, ...rest.slice(at)];
}

/** Inserts a new separator right after the row keyed `afterKey` (Blocking
 *  2's "after the focused row"), or right before the last row when
 *  `afterKey` is `null` ("before the last action when nothing is focused").
 *  `null` when the anchor no longer exists, or the resulting position would
 *  be collapsed by the merge (§1.4) — callers surface that instead of
 *  letting the separator quietly vanish. */
export function insertSeparator(rows: readonly MenuRow[], afterKey: string | null): MenuRow[] | null {
	let at: number;
	if (afterKey) {
		const anchor = rows.findIndex((row) => row.key === afterKey);
		if (anchor < 0) return null;
		at = anchor + 1;
	} else {
		at = Math.max(0, rows.length - 1);
	}
	const row: MenuRow = { kind: 'separator', key: `sep-new-${Date.now()}-${Math.random().toString(36).slice(2, 6)}` };
	const next = [...rows.slice(0, at), row, ...rows.slice(at)];
	return separatorWouldCollapse(next, at) ? null : next;
}

/** Removes the row keyed `key`; `null` when it's already gone (Major 5). */
export function removeRowByKey(rows: readonly MenuRow[], key: string): MenuRow[] | null {
	if (!rows.some((row) => row.key === key)) return null;
	return rows.filter((row) => row.key !== key);
}

export function newActionRow(action: EffectiveAction): MenuRow {
	return {
		kind: 'action',
		key: action.id,
		id: action.id,
		action,
		hidden: false,
		hiddenHere: false,
		hiddenElsewhere: null,
		locked: action.locked,
	};
}

/** Appends `action` to the end of `rows`; `null` when it's already a row
 *  (Major 5: re-checked against *fresh* rows, so a concurrent add of the
 *  same action never doubles it up). */
export function appendActionRow(rows: readonly MenuRow[], action: EffectiveAction): MenuRow[] | null {
	if (rows.some((row) => row.kind === 'action' && row.id === action.id)) return null;
	return [...rows, newActionRow(action)];
}

/** Every action id currently represented in `rows` (visible or hidden) — the
 *  exclusion set for "Add action…", so an action already in this menu never
 *  offers a second, order-losing copy of itself. */
export function menuRowActionIds(rows: readonly MenuRow[]): ReadonlySet<string> {
	return new Set(rows.filter(isActionRow).map((row) => row.id));
}

export type MenuOverridePayload = {
	items: string[];
	hidden: string[];
};

/** `rows` (this scope's intended full order) to the exact `{ items, hidden }`
 *  shape `setMenuOverride` writes (§1.4) — folded into `base`, this scope's
 *  *own* current raw override (`EffectiveMenu.overrides[scope]`; never the
 *  other scope's), rather than replacing it outright (Blocking 3):
 *  - an id in `base.items` that isn't one of `rows` — unresolvable, so it
 *    never became a row at all — is carried forward unseen (§1.4 "an
 *    unknown id ... is kept");
 *  - an id in `base.hidden` that isn't one of `rows` either — unresolvable,
 *    or a hidden id no longer present in this menu's content — is carried
 *    forward the same way ("keep hidden ids that aren't currently in the
 *    menu");
 *  - `hidden` otherwise comes only from rows' own `hiddenHere`, never the
 *    accumulated `hidden` a row displays — so this never copies the other
 *    scope's hidden ids into this one's file. */
export function rowsToOverridePayload(rows: readonly MenuRow[], base: MenuOverride | undefined): MenuOverridePayload {
	const rowIds = new Set(rows.filter(isActionRow).map((row) => row.id));
	const items = rows.map((row) => (row.kind === 'separator' ? SEPARATOR : row.id));
	for (const id of base?.items ?? []) {
		if (typeof id !== 'string' || id === SEPARATOR || rowIds.has(id)) continue;
		items.push(id);
	}

	const hiddenHere = rows.filter(isActionRow).filter((row) => row.hiddenHere).map((row) => row.id);
	const hiddenHereSet = new Set(hiddenHere);
	const preserved = (base?.hidden ?? []).filter((id) => typeof id === 'string' && !rowIds.has(id) && !hiddenHereSet.has(id));
	// Any other key on this scope's own override rides along untouched (§1.4
	// writers never drop what they don't understand).
	return { ...base, items, hidden: [...hiddenHere, ...preserved] };
}
