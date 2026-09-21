// /ngwa/scopes route tests (WP-16a). The real route is mounted in a real
// router; `tauri-cmd` is mocked, so no command touches disk. Each test clicks a
// control and asserts the exact command + arguments, including scope mapping
// (personal → 'workspace', project → `project:<id>`). Fixtures follow the
// golden snapshot's shape (skill paths end in `/SKILL.md`).

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, screen, waitFor, within } from '@testing-library/react';
import * as cmd from '@/lib/tauri-cmd';
import * as home from '@/lib/home';
import { useShellStore } from '@/lib/shell/shell-store';
import { Route as ScopesRoute } from './scopes';
import { HOME, P1_ROOT, P2_ROOT, PROJECTS, mkSnapshot, mountRoutes, scopesItems } from './-ngwa-test-fixtures';

vi.mock('@/lib/registry/use-registry', () => ({
	useRegistryIndex: () => ({ data: undefined, isLoading: false, error: null }),
}));

vi.mock('@/lib/home', () => ({
	loadHome: vi.fn(),
	getHomeSync: () => '',
	shortPath: (p: string) => p,
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

const WRITES = [
	() => m.claudePrimitiveEnable,
	() => m.claudePrimitiveDisable,
	() => m.claudePrimitiveCopy,
	() => m.claudePrimitiveMove,
	() => m.claudePrimitiveRemove,
	() => m.claudePrimitiveEnableFor,
	() => m.claudePrimitiveDisableFor,
	() => m.pkgSetEnabled,
	() => m.pkgUninstall,
];

beforeEach(() => {
	m.ngwaSnapshot.mockResolvedValue(mkSnapshot(scopesItems()));
	vi.mocked(home.loadHome).mockResolvedValue(HOME);
	// biome-ignore lint/suspicious/noExplicitAny: heterogeneous mocks
	for (const f of WRITES) (f() as any).mockResolvedValue(undefined);
	useShellStore.setState({ projects: PROJECTS, activeProjectId: 'p1' } as never);
});

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

async function mount(url = '/ngwa/scopes') {
	const r = mountRoutes([{ route: ScopesRoute, path: '/ngwa/scopes' }], url);
	await screen.findByRole('table', { name: 'Scope and Engine Matrix' });
	// Home resolves asynchronously; wait until path-checked actions are live.
	await waitFor(() => expect(home.loadHome).toHaveBeenCalled());
	await act(async () => {});
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
	return within(menu).getByRole('menuitem', { name }) as HTMLButtonElement;
}

const dialog = () => screen.getByRole('dialog');
const noWrites = () => {
	for (const f of WRITES) expect(f()).not.toHaveBeenCalled();
};

const GW = 'prim:skill:groundwork';
const LINT = 'prim:skill:lint';
const NOTES = 'prim:skill:notes';
const DECK = 'prim:skill:deck';
const EXPLORE = 'prim:agent:explore';
const REVIEWER = 'prim:agent:reviewer';
const RELEASE = 'prim:command:release';
const TASKS = 'pkg:app:com.ikenga.tasks';
const STUDIO = 'pkg:app:com.ikenga.studio';

function unreadable(source: 'engine_config' | 'oba') {
	const snap = mkSnapshot(scopesItems());
	snap.sources[source] = { ok: false, error: 'EACCES', count: 0 };
	return snap;
}

describe('/ngwa/scopes — matrix semantics', () => {
	it('keys rows by (kind, name): the same name in two kinds is two rows and not a conflict', async () => {
		await mount();
		expect(document.querySelector('[data-row="prim:skill:groundwork"]')).not.toBeNull();
		expect(document.querySelector('[data-row="prim:agent:groundwork"]')).not.toBeNull();
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
		expect(cell('prim:agent:groundwork', 'project:p2').getAttribute('data-mark')).toBe('on');
		expect(cell('prim:agent:groundwork', 'project:p1').getAttribute('data-mark')).toBe('none');
	});

	it('flags the overridden_by conflict on the personal cell even with null versions', async () => {
		await mount();
		expect(cell(GW, 'personal').getAttribute('data-mark')).toBe('conflict');
		expect(cell(GW, 'project:p1').getAttribute('data-mark')).toBe('link');
		expect(screen.getByText('groundwork exists twice')).toBeTruthy();
	});

	it('does not call a version mismatch a conflict without overridden_by', async () => {
		const items = scopesItems().map((it) =>
			it.id === 'skill:personal:groundwork'
				? { ...it, version: '0.7.4', placements: it.placements.map((p) => ({ ...p, overridden_by: null })) }
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
		expect(cell(LINT, 'claude').getAttribute('data-mark')).toBe('link');
		expect(cell(LINT, 'codex').getAttribute('data-mark')).toBe('link');
		expect(cell(RELEASE, 'codex').getAttribute('data-mark')).toBe('none');
	});

	it('renders the legend states from the real on-disk nature, not the layout mechanism', async () => {
		await mount();
		expect(cell(LINT, 'personal').getAttribute('data-mark')).toBe('link');
		// golden `com-ikenga-iyke` shape: mechanism symlink-dir, but a real folder.
		expect(cell(NOTES, 'personal').getAttribute('data-mark')).toBe('on');
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
		expect(document.querySelectorAll('tbody tr').length).toBe(4); // groundwork, lint, notes, deck
		const all = Number(document.querySelector('[data-mk="*"] .n')?.textContent);
		expect(all).toBe(12);
		expect(document.querySelector('[data-mk="skill"]')?.getAttribute('aria-pressed')).toBe('true');
	});

	it('kind chips filter the rows and write ?kind= to the URL', async () => {
		const { router } = await mount();
		fireEvent.click(document.querySelector('[data-mk="agent"]') as HTMLElement);
		await waitFor(() => expect(router.state.location.search).toEqual({ kind: 'agent' }));
		expect(document.querySelectorAll('tbody tr').length).toBe(3); // groundwork, explore, reviewer
		fireEvent.click(document.querySelector('[data-mk="*"]') as HTMLElement);
		await waitFor(() => expect(router.state.location.search).toEqual({}));
	});

	it('reads ?search=', async () => {
		await mount('/ngwa/scopes?search=lint');
		expect(document.querySelectorAll('tbody tr').length).toBe(1);
	});

	it('Open Store navigates to the Store filtered to engines', async () => {
		const { router } = await mount();
		fireEvent.click(within(document.querySelector('[data-enginefoot="gemini"]') as HTMLElement).getByRole('button', { name: 'Open Store' }));
		await waitFor(() => expect(router.state.location.pathname).toBe('/ngwa/store'));
		expect(router.state.location.search).toEqual({ kind: 'engine' });
	});

	it('shows the snapshot error, not an empty matrix', async () => {
		m.ngwaSnapshot.mockRejectedValue(new Error('scan exploded'));
		mountRoutes([{ route: ScopesRoute, path: '/ngwa/scopes' }], '/ngwa/scopes');
		expect((await screen.findByRole('alert')).textContent).toContain('scan exploded');
	});
});

describe('/ngwa/scopes — unreadable sources (must-fix 2a)', () => {
	for (const source of ['engine_config', 'oba'] as const) {
		it(`${source} unreadable: cells read unknown, every placing action and every Enable all is disabled`, async () => {
			m.ngwaSnapshot.mockResolvedValue(unreadable(source));
			await mount();
			expect(document.querySelector('[data-conflicts-unknown]')).not.toBeNull();
			expect(document.querySelector('[data-no-conflicts]')).toBeNull();
			for (const [r, c] of [
				[EXPLORE, 'personal'],
				[LINT, 'project:p1'],
				[GW, 'personal'],
				[LINT, 'codex'],
			]) {
				expect(cell(r, c).getAttribute('data-mark')).toBe('unknown');
			}
			const enalls = [...document.querySelectorAll<HTMLButtonElement>('[data-enall]')];
			expect(enalls.length).toBe(5);
			for (const b of enalls) {
				expect(b.disabled).toBe(true);
				expect(b.title).toContain(`${source} unreadable`);
			}
			const menu = openCell(EXPLORE, 'project:p2');
			for (const n of [/^Enable here/, /^Move here/, /^Copy here/, /^Disable/, /^Remove from/]) {
				expect(item(menu, n).disabled).toBe(true);
				expect(item(menu, n).title).toContain('unreadable');
			}
			fireEvent.keyDown(document, { key: 'Escape' });
			const eng = openCell(GW, 'codex');
			expect(item(eng, /^Enable here/).disabled).toBe(true);
			expect(item(eng, /^Disable/).disabled).toBe(true);
			noWrites();
		});
	}
});

describe('/ngwa/scopes — never place over a real file (must-fix 2b)', () => {
	it('a real file in the target cell blocks Enable, Move and Copy with a reason', async () => {
		await mount();
		const menu = openCell(REVIEWER, 'project:p1');
		const en = item(menu, /^Enable here/);
		expect(en.disabled).toBe(true);
		expect(en.title).toBe(
			`A real file is already at ${P1_ROOT}/.claude/agents/reviewer.md; enabling would replace it`
		);
		expect(item(menu, /^Move here/).disabled).toBe(true);
		expect(item(menu, /^Copy here/).disabled).toBe(true);
		expect(item(menu, /^Copy here/).title).toContain(`${P1_ROOT}/.claude/agents/reviewer.md`);
	});

	it('Enable all never includes a row whose target path holds a real file', async () => {
		await mount();
		fireEvent.click(document.querySelector('[data-enall="project:p1"]') as HTMLElement);
		expect(dialog().textContent).toContain('untouched');
		expect(dialog().textContent).not.toContain('reviewer');
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: /^Enable \d+$/ }));
		});
		await waitFor(() => expect(m.claudePrimitiveEnable).toHaveBeenCalled());
		expect(m.claudePrimitiveEnable).not.toHaveBeenCalledWith('agent', 'reviewer', 'project:p1');
		expect(m.claudePrimitiveEnable).toHaveBeenCalledWith('agent', 'explore', 'project:p1');
	});

	it('an unresolved home directory disables personal placing actions instead of guessing', async () => {
		vi.mocked(home.loadHome).mockResolvedValue('');
		await mount();
		const en = item(openCell(EXPLORE, 'personal'), /^Enable here/);
		expect(en.disabled).toBe(true);
		expect(en.title).toContain('root unknown');
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
		const en = item(openCell(LINT, 'personal'), /^Enable here/);
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

	it('refuses Disable on a real file or folder (it would delete it)', async () => {
		await mount();
		const d = item(openCell(RELEASE, 'personal'), /^Disable/);
		expect(d.disabled).toBe(true);
		expect(d.title).toContain('real file');
		fireEvent.keyDown(document, { key: 'Escape' });
		const f = item(openCell(NOTES, 'personal'), /^Disable/);
		expect(f.disabled).toBe(true);
		expect(f.title).toContain('real folder');
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
		fireEvent.click(item(openCell(GW, 'codex'), /^Enable here/));
		await waitFor(() =>
			expect(m.claudePrimitiveEnableFor).toHaveBeenCalledWith('codex', 'skill', 'groundwork', 'workspace', 'shared')
		);
	});

	it('engine Enable for a user-tier engine file is disabled: its path cannot be checked', async () => {
		await mount();
		const en = item(openCell(EXPLORE, 'codex'), /^Enable here/);
		expect(en.disabled).toBe(true);
		expect(en.title).toContain('cannot be checked');
	});

	it('engine column Enable all confirms, then enables exactly the checkable rows', async () => {
		await mount();
		fireEvent.click(document.querySelector('[data-enall="codex"]') as HTMLElement);
		expect(dialog().textContent).toContain('Enable all in codex');
		fireEvent.click(within(dialog()).getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		noWrites();
		fireEvent.click(document.querySelector('[data-enall="codex"]') as HTMLElement);
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Enable 1' }));
		});
		await waitFor(() => expect(m.claudePrimitiveEnableFor).toHaveBeenCalledTimes(1));
		expect(m.claudePrimitiveEnableFor).toHaveBeenCalledWith('codex', 'skill', 'groundwork', 'workspace', 'shared');
	});

	it('engine Disable → claudePrimitiveDisableFor at the exact path it deletes', async () => {
		await mount();
		fireEvent.click(item(openCell(LINT, 'codex'), /^Disable/));
		await waitFor(() =>
			expect(m.claudePrimitiveDisableFor).toHaveBeenCalledWith('codex', 'skill', 'lint', 'project:p2', 'shared')
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

describe('/ngwa/scopes — engine Disable deletes only what it checked (must-fix 3)', () => {
	it('a real folder at the codex path is not offered for Disable (V5)', async () => {
		await mount();
		const d = item(openCell(NOTES, 'codex'), /^Disable/);
		expect(d.disabled).toBe(true);
		expect(d.title).toContain('real folder');
		fireEvent.click(d);
		expect(m.claudePrimitiveDisableFor).not.toHaveBeenCalled();
	});

	it('a placement at a different path than disable_for_core deletes is not offered', async () => {
		await mount();
		const d = item(openCell(DECK, 'codex'), /^Disable/);
		expect(d.disabled).toBe(true);
		expect(d.title).toContain('not at the path');
	});
});

describe('/ngwa/scopes — destructive actions confirm first (DEC-30 / DEC-31)', () => {
	it('Remove a skill names the FOLDER (not SKILL.md), says symlink and store copy survives; cancel calls nothing', async () => {
		await mount();
		fireEvent.click(item(openCell(LINT, 'personal'), /^Remove from Personal/));
		const d = dialog();
		expect(d.querySelector('[data-remove-path]')?.textContent).toBe(`${HOME}/.claude/skills/lint`);
		expect(d.textContent).not.toContain('SKILL.md');
		expect(d.querySelector('[data-nature]')?.getAttribute('data-nature')).toBe('link');
		expect(d.textContent).toContain('the store copy survives');
		fireEvent.click(within(d).getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		noWrites();
	});

	it('Remove a real skill folder says the folder and everything in it are deleted', async () => {
		await mount();
		fireEvent.click(item(openCell(NOTES, 'personal'), /^Remove from Personal/));
		const d = dialog();
		expect(d.querySelector('[data-remove-path]')?.textContent).toBe(`${HOME}/.claude/skills/notes`);
		expect(d.querySelector('[data-nature]')?.getAttribute('data-nature')).toBe('real-dir');
		expect(d.textContent).toContain('and everything in it');
		expect(d.textContent).toContain('deleted permanently');
		await act(async () => {
			fireEvent.click(within(d).getByRole('button', { name: 'Remove' }));
		});
		await waitFor(() => expect(m.claudePrimitiveRemove).toHaveBeenCalledWith('skill', 'notes', 'workspace'));
	});

	it('Remove a real file says it is deleted permanently; confirm calls claudePrimitiveRemove once', async () => {
		await mount();
		fireEvent.click(item(openCell(RELEASE, 'personal'), /^Remove from Personal/));
		const d = dialog();
		expect(d.querySelector('[data-remove-path]')?.textContent).toBe(`${HOME}/.claude/commands/release.md`);
		expect(d.querySelector('[data-nature]')?.getAttribute('data-nature')).toBe('real-file');
		noWrites();
		await act(async () => {
			fireEvent.click(within(d).getByRole('button', { name: 'Remove' }));
		});
		await waitFor(() => expect(m.claudePrimitiveRemove).toHaveBeenCalledTimes(1));
		expect(m.claudePrimitiveRemove).toHaveBeenCalledWith('command', 'release', 'workspace');
	});

	it('Remove in a project maps to project:<id> and names that folder', async () => {
		await mount();
		fireEvent.click(item(openCell(GW, 'project:p1'), /^Remove from royalti-co/));
		expect(dialog().querySelector('[data-remove-path]')?.textContent).toBe(`${P1_ROOT}/.claude/skills/groundwork`);
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Remove' }));
		});
		await waitFor(() =>
			expect(m.claudePrimitiveRemove).toHaveBeenCalledWith('skill', 'groundwork', 'project:p1')
		);
	});

	it('Move confirms first, says the source is deleted and becomes a standalone copy', async () => {
		await mount();
		fireEvent.click(item(openCell(LINT, 'project:p1'), /^Move here/));
		const d = dialog();
		expect(d.querySelector('[data-move-source]')?.textContent).toBe(`${HOME}/.claude/skills/lint`);
		expect(d.textContent).toContain(`${P1_ROOT}/.claude/skills/lint`);
		expect(d.textContent).toContain('deletes the source');
		expect(d.textContent).toContain('standalone copy');
		fireEvent.click(within(d).getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		noWrites();
		fireEvent.click(item(openCell(LINT, 'project:p1'), /^Move here/));
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Move' }));
		});
		await waitFor(() =>
			expect(m.claudePrimitiveMove).toHaveBeenCalledWith('skill', 'lint', 'workspace', 'project:p1')
		);
	});

	it('pkg Remove: cancel calls nothing, confirm calls pkgUninstall', async () => {
		await mount();
		fireEvent.click(item(openCell(TASKS, 'personal'), /^Remove from Personal/));
		expect(dialog().textContent).toContain('/pkgs/tasks');
		fireEvent.click(within(dialog()).getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		noWrites();
		fireEvent.click(item(openCell(TASKS, 'personal'), /^Remove from Personal/));
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Uninstall' }));
		});
		await waitFor(() => expect(m.pkgUninstall).toHaveBeenCalledTimes(1));
		expect(m.pkgUninstall).toHaveBeenCalledWith('com.ikenga.tasks');
	});

	it('Update personal (conflict popover) names the personal FOLDER; cancel nothing; confirm copies project over personal', async () => {
		await mount();
		fireEvent.click(item(openCell(GW, 'personal'), /^Update personal/));
		const d = dialog();
		expect(d.querySelector('[data-overwrite-path]')?.textContent).toBe(`${HOME}/.claude/skills/groundwork`);
		expect(d.textContent).toContain(`${P1_ROOT}/.claude/skills/groundwork`);
		expect(d.textContent).not.toContain('SKILL.md');
		expect(d.textContent).toContain('the store copy survives');
		fireEvent.click(within(d).getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		noWrites();

		fireEvent.click(screen.getByRole('button', { name: 'Update personal' }));
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Overwrite personal' }));
		});
		await waitFor(() => expect(m.claudePrimitiveCopy).toHaveBeenCalledTimes(1));
		expect(m.claudePrimitiveCopy).toHaveBeenCalledWith('skill', 'groundwork', 'project:p1', 'workspace');
	});

	it('Remove from Personal in the conflict popover confirms, then removes from workspace', async () => {
		await mount();
		fireEvent.click(item(openCell(GW, 'personal'), /^Remove from Personal/));
		expect(dialog().querySelector('[data-remove-path]')?.textContent).toBe(`${HOME}/.claude/skills/groundwork`);
		noWrites();
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Remove' }));
		});
		await waitFor(() =>
			expect(m.claudePrimitiveRemove).toHaveBeenCalledWith('skill', 'groundwork', 'workspace')
		);
	});

	it('Remove personal from the side note confirms, then removes from workspace', async () => {
		await mount();
		fireEvent.click(screen.getByRole('button', { name: 'Remove personal' }));
		noWrites();
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Remove' }));
		});
		await waitFor(() =>
			expect(m.claudePrimitiveRemove).toHaveBeenCalledWith('skill', 'groundwork', 'workspace')
		);
	});

	it('Enable all asks first; cancel calls nothing; confirm enables each target', async () => {
		await mount();
		const btn = document.querySelector<HTMLButtonElement>('[data-enall="project:p2"]') as HTMLButtonElement;
		fireEvent.click(btn);
		expect(dialog().textContent).toContain('Enable all in ikenga');
		fireEvent.click(within(dialog()).getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		noWrites();

		fireEvent.click(btn);
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: /^Enable \d+$/ }));
		});
		await waitFor(() => expect(m.claudePrimitiveEnable).toHaveBeenCalledTimes(4));
		for (const [k, n] of [
			['agent', 'explore'],
			['skill', 'groundwork'],
			['agent', 'reviewer'],
			['skill', 'deck'],
		]) {
			expect(m.claudePrimitiveEnable).toHaveBeenCalledWith(k, n, 'project:p2');
		}
		// lint is present in p2 (codex); release and notes are not in the store.
		expect(m.claudePrimitiveEnable).not.toHaveBeenCalledWith('skill', 'lint', 'project:p2');
		expect(P2_ROOT).toBeTruthy();
	});
});
