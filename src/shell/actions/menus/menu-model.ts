// D-06 Menus tab (WP-59): pure row-building and write-payload helpers. Split
// out from the components so the drag/keyboard/eye-toggle logic they drive
// can be unit-tested without React.
//
// G-ACTIONS §1.4's `EffectiveMenu.items` is already hidden-ids-removed
// (`menus.ts` header) — D-06's tree shows hidden items too, greyed with the
// `.off` treatment, so a row list is built by appending `menu.hidden`'s ids
// (resolved through `actionById`) after the visible `items`. This does lose
// a hidden item's *original* interleaved position (the merge never exposes
// the pre-hide order, and G-ACTIONS-API is frozen — nothing here may derive
// it by re-implementing `menus.ts`'s merge), so a row list built this way is
// the write-time order going forward: reordering, hiding or adding always
// writes the *whole* combined list back as one explicit `items` override,
// never a partial patch, so this session's ordering is self-consistent even
// though a hidden item that predates this session may resurface at the end
// rather than its original spot the first time this menu is edited.

import type { EffectiveAction, EffectiveMenu } from '@/lib/actions/store';
import { SEPARATOR } from '@/lib/actions/store';

export type MenuRow =
	| { kind: 'separator'; key: string }
	| { kind: 'action'; key: string; id: string; action: EffectiveAction; hidden: boolean; locked: boolean };

export function isActionRow(row: MenuRow): row is Extract<MenuRow, { kind: 'action' }> {
	return row.kind === 'action';
}

/** The tree's full row list: visible items in their effective order, then
 *  hidden ones appended (see header). An id in `menu.hidden` with no
 *  resolvable action (§1.6 `W_UNKNOWN_COMMAND` — the file names an id no
 *  source defines any more) is dropped: there is nothing to render or move. */
export function buildMenuRows(menu: EffectiveMenu | null, actionById: ReadonlyMap<string, EffectiveAction>): MenuRow[] {
	if (!menu) return [];
	const rows: MenuRow[] = [];
	menu.items.forEach((item, index) => {
		if (item.kind === 'separator') {
			rows.push({ kind: 'separator', key: `sep-${index}` });
			return;
		}
		rows.push({ kind: 'action', key: item.id, id: item.id, action: item.action, hidden: false, locked: item.locked });
	});
	for (const id of menu.hidden) {
		const action = actionById.get(id);
		if (!action) continue;
		rows.push({ kind: 'action', key: `hidden-${id}`, id, action, hidden: true, locked: action.locked });
	}
	return rows;
}

/** Moves the row at `from` to `to`; a no-op (same array) when `to` is out of
 *  range or equal to `from`, so callers can call this unconditionally. */
export function moveRow(rows: readonly MenuRow[], from: number, to: number): MenuRow[] {
	if (to < 0 || to >= rows.length || from === to || from < 0 || from >= rows.length) return rows as MenuRow[];
	const next = rows.slice();
	const [item] = next.splice(from, 1);
	next.splice(to, 0, item);
	return next;
}

/** Every action id currently represented in `rows` (visible or hidden) — the
 *  exclusion set for "Add action…", so an action already in this menu never
 *  offers a second, order-losing copy of itself. */
export function menuRowActionIds(rows: readonly MenuRow[]): ReadonlySet<string> {
	return new Set(rows.filter(isActionRow).map((row) => row.id));
}

export interface MenuOverridePayload {
	items: string[];
	hidden: string[];
}

/** `rows` to the exact `{ items, hidden }` shape `setMenuOverride` writes
 *  (G-ACTIONS §1.4): `items` is the full write-time order (separators as
 *  `"---"`, hidden ids included in position), `hidden` the ids to hide. */
export function rowsToOverridePayload(rows: readonly MenuRow[]): MenuOverridePayload {
	const items = rows.map((row) => (row.kind === 'separator' ? SEPARATOR : row.id));
	const hidden = rows.filter(isActionRow).filter((row) => row.hidden).map((row) => row.id);
	return { items, hidden };
}
