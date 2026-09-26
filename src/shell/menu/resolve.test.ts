// WP-55 — unit tests for `resolveMenuItems` (the pure resolver every
// menu-owning surface calls). Constructs `EffectiveMenu` fixtures directly
// rather than going through the store, so these run with no Tauri bridge and
// no booted actions store — the module under test never touches either.

import { beforeEach, describe, expect, it, vi } from 'vitest';
import { type EffectiveAction, type EffectiveMenu, getEffectiveAction } from '@/lib/actions/store';
import { runAction } from '@/lib/actions/runner';
import { runCommand } from '@/lib/keymap/commands';
import { labelFor } from '@/lib/keymap/registry';
import { menuVariables, resolveMenuItems, runMenuAction } from './resolve';
import { surfaceRunOutcome } from './run-notice';

// Partial mocks spread the real module so every other export (`isHostedCommand`,
// `registerCommand`, `collapseSeparators`, …) stays real.
vi.mock('@/lib/keymap/commands', async (importOriginal) => ({
	...(await importOriginal<typeof import('@/lib/keymap/commands')>()),
	runCommand: vi.fn(),
}));
vi.mock('@/lib/keymap/registry', async (importOriginal) => ({
	...(await importOriginal<typeof import('@/lib/keymap/registry')>()),
	labelFor: vi.fn(() => ''),
}));
vi.mock('@/lib/actions/store', async (importOriginal) => ({
	...(await importOriginal<typeof import('@/lib/actions/store')>()),
	getEffectiveAction: vi.fn(() => undefined),
}));
vi.mock('@/lib/actions/runner', () => ({
	runAction: vi.fn(async () => ({
		status: 'refused',
		kind: 'shell',
		reason: 'untrusted',
		message: 'Not trusted yet',
		trustSheet: { mode: 'project-actions', projectId: 'p1', actionIds: ['lint'] },
	})),
	gatherRunVariables: vi.fn(async () => ({})),
	interpolate: vi.fn((t: string) => t),
}));
vi.mock('./run-notice', () => ({
	surfaceRunOutcome: vi.fn(),
	confirmShellRun: vi.fn(async () => true),
}));

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

	it('falls back to the command table (runMenuAction) for a built-in with no local handler', () => {
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

	it('applies label, icon, destructive and data-action overrides, and a disabled reason', () => {
		const m = menu([
			{ kind: 'action', id: 'viewer.zoom-reset', action: action('viewer.zoom-reset'), layer: 'default', locked: false },
			{ kind: 'action', id: 'pane.close', action: action('pane.close'), layer: 'default', locked: true },
		]);
		const icon = { type: 'svg' } as unknown as import('react').ReactNode;
		const rows = resolveMenuItems(m, {
			labels: { 'viewer.zoom-reset': 'Reset zoom (110%)' },
			icons: { 'viewer.zoom-reset': icon },
			destructive: ['pane.close'],
			dataActions: { 'pane.close': 'close-pane' },
			disabled: (id) => (id === 'pane.close' ? 'Cannot close last pane' : false),
		});
		expect(rows[0]).toMatchObject({ label: 'Reset zoom (110%)', icon, danger: false, dataAction: 'viewer.zoom-reset' });
		expect(rows[1]).toMatchObject({
			danger: true,
			dataAction: 'close-pane',
			disabled: true,
			disabledReason: 'Cannot close last pane',
		});
	});

	it('A-9: with builtinsNeedHandler, a built-in with no local handler is skipped; user and package items stay', () => {
		const m = menu([
			{ kind: 'action', id: 'run-now', action: action('run-now'), layer: 'default', locked: false },
			{ kind: 'action', id: 'open-in-ngwa', action: action('open-in-ngwa'), layer: 'default', locked: false },
			{
				kind: 'action',
				id: 'lint',
				action: action('lint', { source: 'personal' }),
				layer: 'personal',
				locked: false,
			},
			{
				kind: 'action',
				id: 'com.x:do',
				action: action('com.x:do', { source: 'package' }),
				layer: 'package',
				locked: false,
			},
		]);
		const rows = resolveMenuItems(m, { builtinsNeedHandler: true, handlers: { 'open-in-ngwa': () => {} } });
		expect(rows.map((r) => (r.kind === 'item' ? r.id : '---'))).toEqual(['open-in-ngwa', 'lint', 'com.x:do']);
	});

	it('evaluates a placement `when` without a target and without throwing', () => {
		const m = menu([
			{ kind: 'action', id: 'pkg:x', action: action('pkg:x'), layer: 'package', locked: false, when: "resource =~ '*.ts'" },
		]);
		expect(() => resolveMenuItems(m)).not.toThrow();
		expect(resolveMenuItems(m)).toHaveLength(0);
	});

	it('a package placement with an `ngwa-item` kinds `when` shows only on a row of that kind', () => {
		const m = menu([
			{
				kind: 'action',
				id: 'com.x:inspect',
				action: action('com.x:inspect', { source: 'package' }),
				layer: 'package',
				locked: false,
				when: "ngwaItemKind == 'app' || ngwaItemKind == 'tool'",
			},
		]);
		expect(resolveMenuItems(m, { target: { ngwaItemKind: 'tool' } })).toHaveLength(1);
		expect(resolveMenuItems(m, { target: { ngwaItemKind: 'skill' } })).toHaveLength(0);
	});
});

describe('runMenuAction', () => {
	beforeEach(() => {
		vi.clearAllMocks();
	});

	it('menuVariables takes file.path / file.name from the menu target', () => {
		expect(menuVariables({ resource: '/p/src/a.ts' })).toEqual({ 'file.path': '/p/src/a.ts', 'file.name': 'a.ts' });
		expect(menuVariables(undefined)).toEqual({});
		expect(menuVariables({ paneKind: 'terminal' })).toEqual({});
	});

	it('runs a personal action through the WP-53 runner with the row’s variables, and surfaces the outcome', async () => {
		const userAction = { id: 'lint', name: 'Lint file', run: { kind: 'shell', command: 'eslint {{file.path}}' } };
		vi.mocked(getEffectiveAction).mockReturnValue(
			action('lint', { name: 'Lint file', source: 'personal', userAction } as unknown as Partial<EffectiveAction>)
		);
		runMenuAction('lint', { resource: '/p/src/a.ts' });
		await vi.waitFor(() => expect(surfaceRunOutcome).toHaveBeenCalledTimes(1));
		expect(runAction).toHaveBeenCalledWith(
			{ id: 'lint', name: 'Lint file', run: userAction.run, scope: 'personal' },
			expect.objectContaining({ variables: { 'file.path': '/p/src/a.ts', 'file.name': 'a.ts' } })
		);
		expect(vi.mocked(surfaceRunOutcome).mock.calls[0][0]).toMatchObject({ status: 'refused', reason: 'untrusted' });
		expect(runCommand).not.toHaveBeenCalled();
	});

	it('runs a project action as scope "project" (the trust gate applies)', async () => {
		const userAction = { id: 'deploy', name: 'Deploy', run: { kind: 'shell', command: 'make deploy' } };
		vi.mocked(getEffectiveAction).mockReturnValue(
			action('deploy', { name: 'Deploy', source: 'project', userAction } as unknown as Partial<EffectiveAction>)
		);
		runMenuAction('deploy');
		await vi.waitFor(() => expect(runAction).toHaveBeenCalledTimes(1));
		expect(vi.mocked(runAction).mock.calls[0][0]).toMatchObject({ scope: 'project' });
	});

	it('a built-in goes to the command table, which reaches its registered owner', () => {
		vi.mocked(getEffectiveAction).mockReturnValue(action('pane.split-right'));
		runMenuAction('pane.split-right');
		expect(runCommand).toHaveBeenCalledWith({ command: 'pane.split-right', source: 'menu' });
		expect(runAction).not.toHaveBeenCalled();
	});
});
