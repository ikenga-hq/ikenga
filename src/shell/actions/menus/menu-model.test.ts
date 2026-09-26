import { describe, expect, it } from 'vitest';
import type { EffectiveAction, EffectiveMenu } from '@/lib/actions/store';
import { buildMenuRows, menuRowActionIds, moveRow, rowsToOverridePayload } from './menu-model';

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

describe('buildMenuRows', () => {
	it('returns an empty list for a null menu', () => {
		expect(buildMenuRows(null, new Map())).toEqual([]);
	});

	it('renders visible items in order, then appends hidden ids', () => {
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
			overrides: {},
		};
		const rows = buildMenuRows(menu, new Map([['open', open], ['delete', del], ['explain-file', explain]]));
		expect(rows.map((r) => (r.kind === 'separator' ? '---' : r.id))).toEqual(['open', '---', 'delete', 'explain-file']);
		expect(rows[2]).toMatchObject({ kind: 'action', id: 'delete', locked: true, hidden: false });
		expect(rows[3]).toMatchObject({ kind: 'action', id: 'explain-file', hidden: true });
	});

	it('drops a hidden id with no resolvable action (§1.6 W_UNKNOWN_COMMAND)', () => {
		const menu: EffectiveMenu = { id: 'files', items: [], hidden: ['ghost-id'], overrides: {} };
		expect(buildMenuRows(menu, new Map())).toEqual([]);
	});
});

describe('moveRow', () => {
	const rows = [
		{ kind: 'action' as const, key: 'a', id: 'a', action: action('a'), hidden: false, locked: false },
		{ kind: 'action' as const, key: 'b', id: 'b', action: action('b'), hidden: false, locked: false },
		{ kind: 'action' as const, key: 'c', id: 'c', action: action('c'), hidden: false, locked: false },
	];

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

describe('menuRowActionIds', () => {
	it('collects ids from both visible and hidden rows, skipping separators', () => {
		const rows = [
			{ kind: 'separator' as const, key: 'sep-0' },
			{ kind: 'action' as const, key: 'open', id: 'open', action: action('open'), hidden: false, locked: false },
			{ kind: 'action' as const, key: 'x', id: 'explain-file', action: action('explain-file'), hidden: true, locked: false },
		];
		expect(menuRowActionIds(rows)).toEqual(new Set(['open', 'explain-file']));
	});
});

describe('rowsToOverridePayload', () => {
	it('writes separators as "---" and only hidden ids into `hidden`', () => {
		const rows = [
			{ kind: 'action' as const, key: 'open', id: 'open', action: action('open'), hidden: false, locked: false },
			{ kind: 'separator' as const, key: 'sep-1' },
			{ kind: 'action' as const, key: 'x', id: 'explain-file', action: action('explain-file'), hidden: true, locked: false },
		];
		expect(rowsToOverridePayload(rows)).toEqual({
			items: ['open', '---', 'explain-file'],
			hidden: ['explain-file'],
		});
	});
});
