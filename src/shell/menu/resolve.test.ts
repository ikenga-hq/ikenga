// WP-55 — unit tests for `resolveMenuItems` (the pure resolver every
// menu-owning surface calls). Constructs `EffectiveMenu` fixtures directly
// rather than going through the store, so these run with no Tauri bridge and
// no booted actions store — the module under test never touches either.

import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { EffectiveAction, EffectiveMenu } from '@/lib/actions/store';
import { runCommand } from '@/lib/keymap/commands';
import { labelFor } from '@/lib/keymap/registry';
import { resolveMenuItems, runMenuAction } from './resolve';

vi.mock('@/lib/keymap/commands', () => ({ runCommand: vi.fn() }));
vi.mock('@/lib/keymap/registry', () => ({ labelFor: vi.fn(() => '') }));

function action(id: string, overrides: Partial<EffectiveAction> = {}): EffectiveAction {
	return {
		id,
		name: overrides.name ?? id,
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

function menu(items: EffectiveMenu['items']): EffectiveMenu {
	return { id: 'test', items, hidden: [], overrides: {} };
}

describe('resolveMenuItems', () => {
	beforeEach(() => {
		vi.clearAllMocks();
	});

	it('returns [] for a null menu', () => {
		expect(resolveMenuItems(null)).toEqual([]);
	});

	it('maps a plain action item straight through, in order', () => {
		const m = menu([
			{ kind: 'action', id: 'open', action: action('open', { name: 'Open' }), layer: 'default', locked: false },
			{ kind: 'action', id: 'delete', action: action('delete', { name: 'Move to Trash', danger: true }), layer: 'default', locked: true },
		]);
		const rows = resolveMenuItems(m);
		expect(rows).toHaveLength(2);
		expect(rows[0]).toMatchObject({ kind: 'item', id: 'open', label: 'Open', danger: false });
		expect(rows[1]).toMatchObject({ kind: 'item', id: 'delete', label: 'Move to Trash', danger: true, locked: true });
	});

	it('skips an item whose condition does not hold for this open, and collapses the separator it leaves behind', () => {
		const m = menu([
			{ kind: 'action', id: 'a', action: action('a'), layer: 'default', locked: false, condition: 'dir' },
			{ kind: 'separator' },
			{ kind: 'action', id: 'b', action: action('b'), layer: 'default', locked: false },
		]);
		const rows = resolveMenuItems(m, { conditions: { dir: false } });
		expect(rows.map((r) => (r.kind === 'item' ? r.id : '---'))).toEqual(['b']);
	});

	it('keeps an item whose condition holds', () => {
		const m = menu([{ kind: 'action', id: 'a', action: action('a'), layer: 'default', locked: false, condition: 'dir' }]);
		expect(resolveMenuItems(m, { conditions: { dir: true } }).map((r) => (r.kind === 'item' ? r.id : null))).toEqual([
			'a',
		]);
	});

	it('evaluates a placement `when` against the menu context (not conditions)', () => {
		const m = menu([
			{
				kind: 'action',
				id: 'pkg:x',
				action: action('pkg:x'),
				layer: 'package',
				locked: false,
				when: "resource == 'a.ts'",
			},
		]);
		expect(resolveMenuItems(m, { target: { resource: 'a.ts' } })).toHaveLength(1);
		expect(resolveMenuItems(m, { target: { resource: 'a.js' } })).toHaveLength(0);
	});

	it('collapses leading/trailing/doubled separators', () => {
		const m = menu([
			{ kind: 'separator' },
			{ kind: 'action', id: 'a', action: action('a'), layer: 'default', locked: false },
			{ kind: 'separator' },
			{ kind: 'separator' },
			{ kind: 'action', id: 'b', action: action('b'), layer: 'default', locked: false },
			{ kind: 'separator' },
		]);
		const rows = resolveMenuItems(m);
		expect(rows.map((r) => r.kind)).toEqual(['item', 'separator', 'item']);
	});

	it('runs a local handler override instead of the generic fallback', () => {
		const m = menu([{ kind: 'action', id: 'rename', action: action('rename'), layer: 'default', locked: false }]);
		const handler = vi.fn();
		const rows = resolveMenuItems(m, { handlers: { rename: handler } });
		rows[0].kind === 'item' && rows[0].run();
		expect(handler).toHaveBeenCalledTimes(1);
		expect(runCommand).not.toHaveBeenCalled();
	});

	it('falls back to the command table (runMenuAction) when no local handler is given', () => {
		const m = menu([{ kind: 'action', id: 'pkg:view', action: action('pkg:view'), layer: 'package', locked: false }]);
		const rows = resolveMenuItems(m);
		rows[0].kind === 'item' && rows[0].run();
		expect(runCommand).toHaveBeenCalledWith({ command: 'pkg:view', source: 'menu' });
	});

	it('runMenuAction is exactly the runCommand call the fallback uses', () => {
		runMenuAction('foo');
		expect(runCommand).toHaveBeenCalledWith({ command: 'foo', source: 'menu' });
	});

	it('applies the per-id disabled callback', () => {
		const m = menu([{ kind: 'action', id: 'tab.move-left', action: action('tab.move-left'), layer: 'default', locked: false }]);
		const rows = resolveMenuItems(m, { disabled: (id) => id === 'tab.move-left' });
		expect(rows[0]).toMatchObject({ disabled: true });
	});

	it('reads the shortcut label from the keymap registry, not a literal', () => {
		vi.mocked(labelFor).mockReturnValueOnce('⌘K');
		const m = menu([{ kind: 'action', id: 'palette.open', action: action('palette.open'), layer: 'default', locked: false }]);
		expect(resolveMenuItems(m)[0]).toMatchObject({ shortcut: '⌘K' });
	});
});
