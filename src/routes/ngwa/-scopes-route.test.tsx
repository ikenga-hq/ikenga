// /ngwa/scopes route tests (WP-16a). The real route is mounted in a real
// router; `tauri-cmd` is mocked, so no command touches disk. Each test clicks a
// control and asserts the exact command + arguments, including scope mapping
// (personal → 'workspace', project → `project:<id>`).

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, screen, waitFor, within } from '@testing-library/react';
import * as cmd from '@/lib/tauri-cmd';
import { useShellStore } from '@/lib/shell/shell-store';
import { Route as ScopesRoute } from './scopes';
import { PROJECTS, mkSnapshot, mountRoutes, scopesItems } from './-ngwa-test-fixtures';

vi.mock('@/lib/registry/use-registry', () => ({
	useRegistryIndex: () => ({ data: undefined, isLoading: false, error: null }),
}));

vi.mock('@/lib/tauri-cmd', async (orig) => {
	const actual = await orig<typeof import('@/lib/tauri-cmd')>();
	return {
		...actual,
		ngwaSnapshot: vi.fn(),
		claudePrimitiveEnable: vi.fn(),
		claudePrimitiveDisable: vi.fn(),
		claudePrimitiveCopy: vi.fn(),
		claudePrimitiveMove: vi.fn(),
		claudePrimitiveRemove: vi.fn(),
		claudePrimitiveEnableFor: vi.fn(),
		claudePrimitiveDisableFor: vi.fn(),
		pkgSetEnabled: vi.fn(),
		pkgUninstall: vi.fn(),
	};
});

const m = vi.mocked(cmd);

beforeEach(() => {
	m.ngwaSnapshot.mockResolvedValue(mkSnapshot(scopesItems()));
	for (const f of [
		m.claudePrimitiveEnable,
		m.claudePrimitiveDisable,
		m.claudePrimitiveCopy,
		m.claudePrimitiveMove,
		m.claudePrimitiveRemove,
		m.claudePrimitiveEnableFor,
		m.claudePrimitiveDisableFor,
		m.pkgSetEnabled,
		m.pkgUninstall,
	]) {
		// biome-ignore lint/suspicious/noExplicitAny: heterogeneous mocks
		(f as any).mockResolvedValue(undefined);
	}
	useShellStore.setState({
		projects: PROJECTS,
		activeProjectId: 'p1',
	} as never);
});

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

async function mount(url = '/ngwa/scopes') {
	const r = mountRoutes([{ route: ScopesRoute, path: '/ngwa/scopes' }], url);
	await screen.findByRole('table', { name: 'Scope and Engine Matrix' });
	return r;
}

function cell(rowKey: string, col: string): HTMLElement {
	const el = document.querySelector<HTMLElement>(`[data-cell="${rowKey}|${col}"]`);
	if (!el) throw new Error(`no cell ${rowKey}|${col}`);
	return el;
}

function openCell(rowKey: string, col: string) {
	fireEvent.click(cell(rowKey, col));
	return screen.getByRole('menu');
}

function item(menu: HTMLElement, name: RegExp) {
	return within(menu).getByRole('menuitem', { name });
}

const GW = 'prim:skill:groundwork';
const LINT = 'prim:skill:lint';
const EXPLORE = 'prim:agent:explore';
const RELEASE = 'prim:command:release';
const TASKS = 'pkg:app:com.ikenga.tasks';
const STUDIO = 'pkg:app:com.ikenga.studio';

describe('/ngwa/scopes — matrix semantics', () => {
	it('keys rows by (kind, name): the same name in two kinds is two rows and not a conflict', async () => {
		await mount();
		expect(document.querySelector('[data-row="prim:skill:groundwork"]')).not.toBeNull();
		expect(document.querySelector('[data-row="prim:agent:groundwork"]')).not.toBeNull();
		// Only the skill is in conflict.
		expect(document.querySelectorAll('[data-conflict]').length).toBe(1);
		expect(cell('prim:agent:groundwork', 'personal').getAttribute('data-mark')).toBe('none');
	});

	it('draws one column per registered project, active project first, filtered by project_id', async () => {
		await mount();
		const heads = [...document.querySelectorAll('thead th[data-col]')].map((th) =>
			th.getAttribute('data-col')
		);
		expect(heads).toEqual(['personal', 'project:p1', 'project:p2', 'claude', 'codex']);
		expect(document.querySelector('th[data-col="project:p1"]')?.className).toContain('active');
		// The p2 agent appears only in the p2 column.
		expect(cell('prim:agent:groundwork', 'project:p2').getAttribute('data-mark')).toBe('on');
		expect(cell('prim:agent:groundwork', 'project:p1').getAttribute('data-mark')).toBe('none');
	});

	it('flags the overridden_by conflict on the personal cell even with null versions', async () => {
		await mount();
		expect(cell(GW, 'personal').getAttribute('data-mark')).toBe('conflict');
		expect(cell(GW, 'project:p1').getAttribute('data-mark')).toBe('on');
		expect(screen.getByText('groundwork exists twice')).toBeTruthy();
	});

	it('does not call a version mismatch a conflict without overridden_by', async () => {
		const items = scopesItems().map((it) =>
			it.id === 'skill:personal:groundwork'
				? {
						...it,
						version: '0.7.4',
						placements: it.placements.map((p) => ({ ...p, overridden_by: null })),
					}
				: it.id === 'skill:project:p1:groundwork'
					? { ...it, version: '0.7.6' }
					: it
		);
		m.ngwaSnapshot.mockResolvedValue(mkSnapshot(items));
		await mount();
		expect(cell(GW, 'personal').getAttribute('data-mark')).toBe('link');
		expect(document.querySelector('[data-no-conflicts]')).not.toBeNull();
	});

	it('reads engine cells as the union of the row placements', async () => {
		await mount();
		// lint: claude placement on the personal item, codex placement on the p2 item.
		expect(cell(LINT, 'claude').getAttribute('data-mark')).toBe('link');
		expect(cell(LINT, 'codex').getAttribute('data-mark')).toBe('link');
		expect(cell(RELEASE, 'codex').getAttribute('data-mark')).toBe('none');
	});

	it('renders the symlinked legend state in cells', async () => {
		await mount();
		expect(cell(LINT, 'personal').getAttribute('data-mark')).toBe('link');
		expect(cell(RELEASE, 'personal').getAttribute('data-mark')).toBe('on');
		expect(cell(EXPLORE, 'personal').getAttribute('data-mark')).toBe('off');
	});

	it('hides an uninstalled engine column and says so', async () => {
		await mount();
		expect(document.querySelector('th[data-col="gemini"]')).toBeNull();
		expect(document.querySelector('[data-enginefoot="gemini"]')).not.toBeNull();
		expect(document.querySelector('[data-enginefoot="codex"]')).toBeNull();
	});

	it('makes the codex column conditional on the codex engine pkg too', async () => {
		m.ngwaSnapshot.mockResolvedValue(
			mkSnapshot(scopesItems().filter((it) => it.id !== 'com.ikenga.engine-codex'))
		);
		await mount();
		expect(document.querySelector('th[data-col="codex"]')).toBeNull();
		expect(document.querySelector('[data-enginefoot="codex"]')).not.toBeNull();
	});

	it('the all chip shows the unfiltered count, and ?kind= is read', async () => {
		await mount('/ngwa/scopes?kind=skill');
		const all = document.querySelector('[data-mk="*"] .n');
		// 11 items → 10 rows (groundwork skill + lint merge across scopes; 2 engines, …)
		const rowsTotal = Number(all?.textContent);
		expect(document.querySelectorAll('tbody tr').length).toBe(2); // groundwork + lint skills
		expect(rowsTotal).toBeGreaterThan(2);
		expect(document.querySelector('[data-mk="skill"]')?.getAttribute('aria-pressed')).toBe('true');
	});

	it('reads ?search=', async () => {
		await mount('/ngwa/scopes?search=lint');
		expect(document.querySelectorAll('tbody tr').length).toBe(1);
	});

	it('renders an unreadable source as unknown, never as "no conflicts"', async () => {
		m.ngwaSnapshot.mockResolvedValue(
			mkSnapshot(scopesItems(), {
				sources: {
					kernel: { ok: true, error: null, count: 1 },
					oba: { ok: true, error: null, count: 1 },
					engine_config: { ok: false, error: 'EACCES', count: 0 },
					engine_assets: { ok: true, error: null, count: 1 },
					trust: { ok: true, error: null, count: 1 },
					usage: { ok: true, error: null, count: 1 },
				},
			})
		);
		await mount();
		expect(document.querySelector('[data-conflicts-unknown]')).not.toBeNull();
		expect(document.querySelector('[data-no-conflicts]')).toBeNull();
		expect(document.querySelector('[data-unreadable]')?.textContent).toContain('engine_config');
	});

	it('shows the snapshot error, not an empty matrix', async () => {
		m.ngwaSnapshot.mockRejectedValue(new Error('scan exploded'));
		mountRoutes([{ route: ScopesRoute, path: '/ngwa/scopes' }], '/ngwa/scopes');
		expect((await screen.findByRole('alert')).textContent).toContain('scan exploded');
	});
});

describe('/ngwa/scopes — popover behaviour', () => {
	it('uses the D-02 labels and closes on Esc', async () => {
		await mount();
		const menu = openCell(LINT, 'project:p1');
		for (const n of [/^Enable here/, /^Move here/, /^Copy here/, /^Disable/, /^Remove from royalti-co/]) {
			expect(item(menu, n)).toBeTruthy();
		}
		fireEvent.keyDown(document, { key: 'Escape' });
		expect(screen.queryByRole('menu')).toBeNull();
	});

	it('closes on an outside click', async () => {
		await mount();
		openCell(LINT, 'project:p1');
		fireEvent.mouseDown(document.body);
		expect(screen.queryByRole('menu')).toBeNull();
	});

	it('disables with a reason instead of going inert', async () => {
		await mount();
		const menu = openCell(LINT, 'personal');
		const en = item(menu, /^Enable here/) as HTMLButtonElement;
		expect(en.disabled).toBe(true);
		expect(en.title).toBe('Already enabled here');
	});
});

describe('/ngwa/scopes — actions call the real commands', () => {
	it('Enable here in a project → claudePrimitiveEnable(kind, name, project:<id>) and refetches the snapshot', async () => {
		await mount();
		const before = m.ngwaSnapshot.mock.calls.length;
		fireEvent.click(item(openCell(EXPLORE, 'project:p2'), /^Enable here/));
		await waitFor(() =>
			expect(m.claudePrimitiveEnable).toHaveBeenCalledWith('agent', 'explore', 'project:p2')
		);
		await waitFor(() => expect(m.ngwaSnapshot.mock.calls.length).toBeGreaterThan(before));
	});

	it('Enable here in personal maps to workspace', async () => {
		await mount();
		fireEvent.click(item(openCell(EXPLORE, 'personal'), /^Enable here/));
		await waitFor(() =>
			expect(m.claudePrimitiveEnable).toHaveBeenCalledWith('agent', 'explore', 'workspace')
		);
	});

	it('Disable a store link → claudePrimitiveDisable', async () => {
		await mount();
		fireEvent.click(item(openCell(LINT, 'personal'), /^Disable/));
		await waitFor(() =>
			expect(m.claudePrimitiveDisable).toHaveBeenCalledWith('skill', 'lint', 'workspace')
		);
	});

	it('refuses Disable on a real file (it would delete it)', async () => {
		await mount();
		const d = item(openCell(RELEASE, 'personal'), /^Disable/) as HTMLButtonElement;
		expect(d.disabled).toBe(true);
		expect(d.title).toContain('real file');
	});

	it('Move here → claudePrimitiveMove(from personal to project)', async () => {
		await mount();
		fireEvent.click(item(openCell(LINT, 'project:p1'), /^Move here/));
		await waitFor(() =>
			expect(m.claudePrimitiveMove).toHaveBeenCalledWith('skill', 'lint', 'workspace', 'project:p1')
		);
	});

	it('Copy here → claudePrimitiveCopy(from personal to project)', async () => {
		await mount();
		fireEvent.click(item(openCell(RELEASE, 'project:p2'), /^Copy here/));
		await waitFor(() =>
			expect(m.claudePrimitiveCopy).toHaveBeenCalledWith('command', 'release', 'workspace', 'project:p2')
		);
	});

	it('engine cell Enable here → claudePrimitiveEnableFor(engine, kind, name, workspace)', async () => {
		await mount();
		// explore is store-backed and placed nowhere.
		fireEvent.click(item(openCell(EXPLORE, 'codex'), /^Enable here/));
		await waitFor(() =>
			expect(m.claudePrimitiveEnableFor).toHaveBeenCalledWith(
				'codex',
				'agent',
				'explore',
				'workspace',
				'shared'
			)
		);
	});

	it('engine cell Disable → claudePrimitiveDisableFor in the placement scope', async () => {
		await mount();
		fireEvent.click(item(openCell(LINT, 'codex'), /^Disable/));
		await waitFor(() =>
			expect(m.claudePrimitiveDisableFor).toHaveBeenCalledWith(
				'codex',
				'skill',
				'lint',
				'project:p2',
				'shared'
			)
		);
	});

	it('pkg Disable / Enable → pkgSetEnabled', async () => {
		await mount();
		fireEvent.click(item(openCell(TASKS, 'personal'), /^Disable/));
		await waitFor(() => expect(m.pkgSetEnabled).toHaveBeenCalledWith('com.ikenga.tasks', false));
		fireEvent.click(item(openCell(STUDIO, 'project:p1'), /^Enable here/));
		await waitFor(() => expect(m.pkgSetEnabled).toHaveBeenCalledWith('com.ikenga.studio', true));
	});

	it('shows a failed command as an error', async () => {
		m.claudePrimitiveEnable.mockRejectedValueOnce(new Error('store entry missing'));
		await mount();
		fireEvent.click(item(openCell(EXPLORE, 'personal'), /^Enable here/));
		const st = await waitFor(() => {
			const el = document.querySelector('[data-mstatus]');
			if (!el) throw new Error('no status');
			return el;
		});
		expect(st.className).toContain('err');
		expect(st.textContent).toContain('store entry missing');
	});
});

describe('/ngwa/scopes — destructive actions confirm first (DEC-30 / DEC-31)', () => {
	function dialog() {
		return screen.getByRole('dialog');
	}

	it('Remove from Personal names the path and the symlink; cancel calls nothing', async () => {
		await mount();
		fireEvent.click(item(openCell(LINT, 'personal'), /^Remove from Personal/));
		const d = dialog();
		expect(d.querySelector('[data-remove-path]')?.textContent).toBe('/home/.claude/skills/lint');
		expect(d.textContent).toContain('symlink');
		expect(d.textContent).toContain('store copy survives');
		fireEvent.click(within(d).getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(m.claudePrimitiveRemove).not.toHaveBeenCalled();
	});

	it('Remove a real file says it is deleted permanently; confirm calls claudePrimitiveRemove once', async () => {
		await mount();
		fireEvent.click(item(openCell(RELEASE, 'personal'), /^Remove from Personal/));
		const d = dialog();
		expect(d.textContent).toContain('real file');
		expect(d.textContent).toContain('deleted permanently');
		expect(m.claudePrimitiveRemove).not.toHaveBeenCalled();
		await act(async () => {
			fireEvent.click(within(d).getByRole('button', { name: 'Remove' }));
		});
		await waitFor(() => expect(m.claudePrimitiveRemove).toHaveBeenCalledTimes(1));
		expect(m.claudePrimitiveRemove).toHaveBeenCalledWith('command', 'release', 'workspace');
	});

	it('Remove in a project maps to project:<id>', async () => {
		await mount();
		fireEvent.click(item(openCell(GW, 'project:p1'), /^Remove from royalti-co/));
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Remove' }));
		});
		await waitFor(() =>
			expect(m.claudePrimitiveRemove).toHaveBeenCalledWith('skill', 'groundwork', 'project:p1')
		);
	});

	it('pkg Remove: cancel calls nothing, confirm calls pkgUninstall', async () => {
		await mount();
		fireEvent.click(item(openCell(TASKS, 'personal'), /^Remove from Personal/));
		expect(dialog().textContent).toContain('/pkgs/tasks');
		fireEvent.click(within(dialog()).getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(m.pkgUninstall).not.toHaveBeenCalled();
		fireEvent.click(item(openCell(TASKS, 'personal'), /^Remove from Personal/));
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Uninstall' }));
		});
		await waitFor(() => expect(m.pkgUninstall).toHaveBeenCalledTimes(1));
		expect(m.pkgUninstall).toHaveBeenCalledWith('com.ikenga.tasks');
	});

	it('Update personal (conflict popover): cancel calls nothing, confirm copies project over personal', async () => {
		await mount();
		const menu = openCell(GW, 'personal');
		fireEvent.click(item(menu, /^Update personal/));
		const d = dialog();
		expect(d.querySelector('[data-overwrite-path]')?.textContent).toBe('/home/.claude/skills/groundwork');
		fireEvent.click(within(d).getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(m.claudePrimitiveCopy).not.toHaveBeenCalled();

		fireEvent.click(screen.getByRole('button', { name: 'Update personal' }));
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Overwrite personal' }));
		});
		await waitFor(() => expect(m.claudePrimitiveCopy).toHaveBeenCalledTimes(1));
		expect(m.claudePrimitiveCopy).toHaveBeenCalledWith('skill', 'groundwork', 'project:p1', 'workspace');
	});

	it('Remove personal from the side note confirms, then removes from workspace', async () => {
		await mount();
		fireEvent.click(screen.getByRole('button', { name: 'Remove personal' }));
		expect(m.claudePrimitiveRemove).not.toHaveBeenCalled();
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Remove' }));
		});
		await waitFor(() =>
			expect(m.claudePrimitiveRemove).toHaveBeenCalledWith('skill', 'groundwork', 'workspace')
		);
	});

	it('Enable all asks first; cancel calls nothing; confirm enables each target', async () => {
		await mount();
		const btn = document.querySelector<HTMLButtonElement>('[data-enall="project:p2"]');
		expect(btn).not.toBeNull();
		fireEvent.click(btn as HTMLButtonElement);
		const d = dialog();
		expect(d.textContent).toContain('Enable all in ikenga');
		fireEvent.click(within(d).getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(m.claudePrimitiveEnable).not.toHaveBeenCalled();

		fireEvent.click(btn as HTMLButtonElement);
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: /^Enable \d+$/ }));
		});
		// Store-backed rows absent from p2: explore (agent) and groundwork (skill).
		await waitFor(() =>
			expect(m.claudePrimitiveEnable).toHaveBeenCalledWith('agent', 'explore', 'project:p2')
		);
		expect(m.claudePrimitiveEnable).toHaveBeenCalledWith('skill', 'groundwork', 'project:p2');
		// lint is present in p2 (codex placement) — untouched. release is not store-backed.
		expect(m.claudePrimitiveEnable).not.toHaveBeenCalledWith('command', 'release', 'project:p2');
	});
});
