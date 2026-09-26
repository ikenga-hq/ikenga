import { describe, expect, it } from 'vitest';
import type { EffectiveAction, EffectiveMenu } from '@/lib/actions/store';
import {
	appendActionRow,
	buildMenuRows,
	insertSeparator,
	menuRowActionIds,
	moveRow,
	moveRowAfter,
	removeRowByKey,
	rowsToOverridePayload,
	separatorWouldCollapse,
} from './menu-model';

function action(id: string, overrides: Partial<EffectiveAction> = {}): EffectiveAction {
	return {
		id,
		name: id,
		description: '',
		source: 'builtin',
		run: { kind: 'builtin' },
		placements: [],
		locked: false,
		danger: false,
		hosted: false,
		osOnly: false,
		editable: false,
		...overrides,
	};
}

function actionRow(id: string, overrides: Partial<Extract<ReturnType<typeof buildMenuRows>[number], { kind: 'action' }>> = {}) {
	return {
		kind: 'action' as const,
		key: id,
		id,
		action: action(id),
		hidden: false,
		hiddenHere: false,
		hiddenElsewhere: null,
		locked: false,
		...overrides,
	};
}

describe('buildMenuRows', () => {
	it('returns an empty list for a null menu', () => {
		expect(buildMenuRows(null, new Map(), 'personal')).toEqual([]);
	});

	it('renders visible items in order, then appends hidden ids, when this scope has no `items` override', () => {
		const open = action('open');
		const del = action('delete', { locked: true });
		const explain = action('explain-file');
		const menu: EffectiveMenu = {
			id: 'files',
			items: [
				{ kind: 'action', id: 'open', action: open, layer: 'default', locked: false },
				{ kind: 'separator' },
				{ kind: 'action', id: 'delete', action: del, layer: 'default', locked: true },
			],
			hidden: ['explain-file'],
			overrides: { personal: { hidden: ['explain-file'] } },
		};
		const rows = buildMenuRows(menu, new Map([['open', open], ['delete', del], ['explain-file', explain]]), 'personal');
		expect(rows.map((r) => (r.kind === 'separator' ? '---' : r.id))).toEqual(['open', '---', 'delete', 'explain-file']);
		expect(rows[2]).toMatchObject({ kind: 'action', id: 'delete', locked: true, hidden: false });
		expect(rows[3]).toMatchObject({ kind: 'action', id: 'explain-file', hidden: true, hiddenHere: true, hiddenElsewhere: null });
	});

	it('drops a hidden id with no resolvable action (§1.6 W_UNKNOWN_COMMAND)', () => {
		const menu: EffectiveMenu = { id: 'files', items: [], hidden: ['ghost-id'], overrides: {} };
		expect(buildMenuRows(menu, new Map(), 'personal')).toEqual([]);
	});

	it('flags a row hidden by the other scope, not this one, as `hiddenElsewhere` (review round 1 minor)', () => {
		const open = action('open');
		const menu: EffectiveMenu = {
			id: 'files',
			items: [],
			hidden: ['open'],
			overrides: { project: { hidden: ['open'] } },
		};
		const rows = buildMenuRows(menu, new Map([['open', open]]), 'personal');
		expect(rows).toMatchObject([{ id: 'open', hidden: true, hiddenHere: false, hiddenElsewhere: 'project' }]);
	});

	it("interleaves a hidden row back among visible ones from this scope's own `items` order, not always at the end (review round 1 major 4)", () => {
		const a = action('a');
		const b = action('b');
		const c = action('c');
		const menu: EffectiveMenu = {
			id: 'files',
			items: [
				{ kind: 'action', id: 'a', action: a, layer: 'personal', locked: false },
				{ kind: 'action', id: 'c', action: c, layer: 'personal', locked: false },
			],
			hidden: ['b'],
			overrides: { personal: { items: ['a', 'b', 'c'], hidden: ['b'] } },
		};
		const rows = buildMenuRows(menu, new Map([['a', a], ['b', b], ['c', c]]), 'personal');
		expect(rows.map((r) => (r.kind === 'action' ? r.id : '---'))).toEqual(['a', 'b', 'c']);
	});

	it('matches separators positionally between this scope\'s `items` and the effective menu, rather than by id', () => {
		const a = action('a');
		const b = action('b');
		const menu: EffectiveMenu = {
			id: 'files',
			items: [
				{ kind: 'action', id: 'a', action: a, layer: 'personal', locked: false },
				{ kind: 'separator' },
				{ kind: 'action', id: 'b', action: b, layer: 'personal', locked: false },
			],
			hidden: [],
			overrides: { personal: { items: ['b', '---', 'a'] } },
		};
		const rows = buildMenuRows(menu, new Map([['a', a], ['b', b]]), 'personal');
		expect(rows.map((r) => (r.kind === 'separator' ? '---' : r.id))).toEqual(['b', '---', 'a']);
	});
});

describe('moveRow', () => {
	const rows = [actionRow('a'), actionRow('b'), actionRow('c')];

	it('moves an item to a later index', () => {
		const next = moveRow(rows, 0, 2);
		expect(next.map((r) => r.key)).toEqual(['b', 'c', 'a']);
	});

	it('moves an item to an earlier index', () => {
		const next = moveRow(rows, 2, 0);
		expect(next.map((r) => r.key)).toEqual(['c', 'a', 'b']);
	});

	it('is a no-op past either end, so callers need no bounds check', () => {
		expect(moveRow(rows, 0, -1)).toBe(rows);
		expect(moveRow(rows, 2, 3)).toBe(rows);
		expect(moveRow(rows, 1, 1)).toBe(rows);
	});
});

describe('separatorWouldCollapse (review round 1 blocking 2)', () => {
	const rows = [actionRow('a'), { kind: 'separator' as const, key: 'sep-0' }, actionRow('b')];

	it('rejects a leading position', () => {
		expect(separatorWouldCollapse(rows, 0)).toBe(true);
	});

	it('rejects a trailing position', () => {
		expect(separatorWouldCollapse(rows, rows.length - 1)).toBe(true);
	});

	it('rejects sitting next to another separator', () => {
		const withTwo = [actionRow('a'), { kind: 'separator' as const, key: 'sep-0' }, { kind: 'separator' as const, key: 'sep-1' }, actionRow('b')];
		expect(separatorWouldCollapse(withTwo, 2)).toBe(true);
	});

	it('accepts a position strictly between two other rows', () => {
		expect(separatorWouldCollapse(rows, 1)).toBe(false);
	});
});

describe('insertSeparator', () => {
	const rows = [actionRow('a'), actionRow('b'), actionRow('c')];

	it('inserts after the given anchor row', () => {
		const next = insertSeparator(rows, 'a');
		expect(next?.map((r) => r.key)).toEqual(['a', expect.stringMatching(/^sep-new-/), 'b', 'c']);
	});

	it('inserts before the last row when there is no anchor (nothing focused)', () => {
		const next = insertSeparator(rows, null);
		expect(next?.map((r) => (r.kind === 'separator' ? '---' : r.key))).toEqual(['a', 'b', '---', 'c']);
	});

	it('refuses a position that would collapse (anchor is the last row)', () => {
		expect(insertSeparator(rows, 'c')).toBeNull();
	});

	it('refuses an anchor that no longer exists', () => {
		expect(insertSeparator(rows, 'ghost')).toBeNull();
	});
});

describe('moveRowAfter (review round 1 major 5)', () => {
	const rows = [actionRow('a'), actionRow('b'), actionRow('c')];

	it('moves the keyed row to sit after the anchor', () => {
		const next = moveRowAfter(rows, 'a', 'c');
		expect(next?.map((r) => r.key)).toEqual(['b', 'c', 'a']);
	});

	it('moves to the start when the anchor is null', () => {
		const next = moveRowAfter(rows, 'c', null);
		expect(next?.map((r) => r.key)).toEqual(['c', 'a', 'b']);
	});

	it('returns null when the moved row is gone (a stale move re-applied to fresh rows)', () => {
		expect(moveRowAfter(rows, 'ghost', 'a')).toBeNull();
	});
});

describe('removeRowByKey', () => {
	it('removes the row and returns null if it is already gone', () => {
		const rows = [actionRow('a'), actionRow('b')];
		expect(removeRowByKey(rows, 'a')?.map((r) => r.key)).toEqual(['b']);
		expect(removeRowByKey(rows, 'ghost')).toBeNull();
	});
});

describe('appendActionRow', () => {
	it('appends a new action and refuses a duplicate', () => {
		const rows = [actionRow('a')];
		const b = action('b');
		const appended = appendActionRow(rows, b);
		expect(appended?.map((r) => r.key)).toEqual(['a', 'b']);
		expect(appendActionRow(rows, action('a'))).toBeNull();
	});
});

describe('menuRowActionIds', () => {
	it('collects ids from both visible and hidden rows, skipping separators', () => {
		const rows = [{ kind: 'separator' as const, key: 'sep-0' }, actionRow('open'), actionRow('explain-file', { hidden: true, hiddenHere: true })];
		expect(menuRowActionIds(rows)).toEqual(new Set(['open', 'explain-file']));
	});
});

describe('rowsToOverridePayload (review round 1 blocking 3)', () => {
	it('writes separators as "---" and only this-scope-hidden ids into `hidden`, with no base to fold in', () => {
		const rows = [actionRow('open'), { kind: 'separator' as const, key: 'sep-1' }, actionRow('explain-file', { hidden: true, hiddenHere: true })];
		expect(rowsToOverridePayload(rows, undefined)).toEqual({
			items: ['open', '---', 'explain-file'],
			hidden: ['explain-file'],
		});
	});

	it("keeps an unknown id from this scope's own `items` that never became a row", () => {
		const rows = [actionRow('open')];
		const payload = rowsToOverridePayload(rows, { items: ['open', 'uninstalled-pkg:thing'] });
		expect(payload.items).toEqual(['open', 'uninstalled-pkg:thing']);
	});

	it("keeps a hidden id from this scope's own override that isn't part of this menu's current rows", () => {
		const rows = [actionRow('open')];
		const payload = rowsToOverridePayload(rows, { hidden: ['gone-from-this-menu'] });
		expect(payload.hidden).toEqual(['gone-from-this-menu']);
	});

	it("never copies the other scope's hidden ids into this scope's write", () => {
		// A row hidden by the *other* scope (`hiddenHere: false`) must not
		// appear in `hidden` just because the row displays as hidden overall.
		const rows = [actionRow('open'), actionRow('explain-file', { hidden: true, hiddenHere: false, hiddenElsewhere: 'project' })];
		const payload = rowsToOverridePayload(rows, { hidden: [] });
		expect(payload.hidden).toEqual([]);
	});

	it('does not duplicate an id that is both hidden-here and already listed in the stale base', () => {
		const rows = [actionRow('open', { hidden: true, hiddenHere: true })];
		const payload = rowsToOverridePayload(rows, { hidden: ['open'] });
		expect(payload.hidden).toEqual(['open']);
	});
});
