// Ngwa Health surface tests (WP-16a). The panels are covered route-level in
// src/routes/ngwa/-health-route.test.tsx; this file holds the helpers and the
// "one engine signal" check across both screens (must-fix 3).

import { afterEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import * as cmd from '@/lib/tauri-cmd';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { handToChi } from '@/shell/companion/companion-store';
import {
	NgwaHealthSurface,
	fmtBytes,
	fmtTime,
	issueLabel,
	summarizeRemove,
	summarizeRemoveAll,
} from './ngwa-health-surface';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { NgwaScopesSurface, type NgwaScopeActions } from './ngwa-scopes-surface';
import { engineItems, mkItem, mkPlacement, mkSnapshot } from '@/routes/ngwa/-ngwa-test-fixtures';

vi.mock('@/lib/tauri-cmd', async (orig) => {
	const actual = await orig<typeof import('@/lib/tauri-cmd')>();
	const never = () => new Promise(() => {});
	return {
		...actual,
		pkgPermissionViolationsList: vi.fn(never),
		pkgHealthScan: vi.fn(never),
		pkgKernelStatus: vi.fn(never),
		agentOpsListJobs: vi.fn(never),
		backupList: vi.fn(never),
		detectAgent: vi.fn(never),
		pkgHealthRemove: vi.fn(),
		pkgHealthRemoveAll: vi.fn(),
		isRemoteWebSession: vi.fn(() => false),
	};
});

vi.mock('@/shell/companion/companion-store', async (orig) => {
	const actual = await orig<typeof import('@/shell/companion/companion-store')>();
	return { ...actual, handToChi: vi.fn() };
});

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

const noop = () => Promise.resolve();
const actions: NgwaScopeActions = {
	enable: noop,
	disable: noop,
	copy: noop,
	move: noop,
	remove: noop,
	enableFor: noop,
	disableFor: noop,
	pkgSetEnabled: noop,
	pkgUninstall: noop,
	openStore: () => {},
};

describe('health helpers', () => {
	it('never turns an unmeasured value into a number', () => {
		expect(fmtTime(null)).toBe('—');
		expect(fmtBytes(null)).toBe('absent');
		expect(fmtBytes(0)).toBe('0 B');
	});
	it('labels an orphan row by its table, not as a broken install', () => {
		expect(issueLabel({ kind: 'orphan_row', table: 'pkg_settings' })).toBe('orphan: pkg_settings');
		expect(issueLabel({ kind: 'manifest_missing' })).toBe('missing manifest');
	});
});

describe('one engine signal for both screens', () => {
	for (const installed of [
		['claude'] as const,
		['claude', 'gemini'] as const,
		['claude', 'codex', 'gemini'] as const,
	]) {
		it(`agrees for engine pkgs: ${installed.join(', ')}`, async () => {
			// codex placements exist even when the codex engine pkg does not.
			const items = [
				...engineItems([...installed]),
				mkItem({
					id: 'skill:personal:s',
					kind: 'skill',
					name: 's',
					placements: [
						mkPlacement({ path: '/c/s' }),
						mkPlacement({ engine: 'codex', path: '/x/s' }),
						mkPlacement({ engine: 'gemini', path: '/g/s' }),
					],
				}),
			];
			const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
			const { container } = render(
				<QueryClientProvider client={qc}>
					<NgwaScopesSurface
						items={items}
						scopes={[{ key: 'personal', label: 'Personal', sub: '~/.claude', active: false }]}
						actions={actions}
					/>
					<NgwaHealthSurface
						items={items}
						snapshot={mkSnapshot(items)}
						onOpenBackup={() => {}}
						onOpenStore={() => {}}
					/>
				</QueryClientProvider>
			);
			const cols = [...container.querySelectorAll('thead th.eng')].map((th) => th.getAttribute('data-col'));
			const healthInstalled = ['claude', 'codex', 'gemini'].filter(
				(e) => !container.querySelector(`[data-engine="${e}"] [data-act="installengine"]`)
			);
			expect(cols).toEqual([...installed].sort((a, b) => ['claude', 'codex', 'gemini'].indexOf(a) - ['claude', 'codex', 'gemini'].indexOf(b)));
			await waitFor(() => expect(healthInstalled).toEqual(cols));
			expect(container.querySelector('[data-enginen]')?.textContent).toBe(`${installed.length} of 3`);
		});
	}
});

describe('a pkg on disk that failed to register (Bug 2)', () => {
	const MEETINGS: cmd.PkgHealthIssue = {
		id: 'com.ikenga.meetings',
		install_path: 'C:/pkgs/com.ikenga.meetings',
		enabled: false,
		issue: { kind: 'pkgs_dir_unloadable' },
		detail:
			'on disk but failed to load: `ui.nav` was removed in manifest v5 (G-MANIFEST-V5 §4 / DEC-37) — declare `ui.views[]` instead',
	};

	function mount(props: { canReinstall?: (id: string) => boolean; onReinstall?: (id: string) => void }) {
		vi.mocked(cmd.pkgHealthScan).mockResolvedValueOnce([MEETINGS]);
		const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
		return render(
			<QueryClientProvider client={qc}>
				<NgwaHealthSurface
					items={[]}
					snapshot={mkSnapshot([])}
					onOpenBackup={() => {}}
					onOpenStore={() => {}}
					{...props}
				/>
			</QueryClientProvider>
		);
	}

	it('labels the new kinds', () => {
		expect(issueLabel({ kind: 'pkgs_dir_unloadable' })).toBe('failed to load');
		expect(issueLabel({ kind: 'register_failed' })).toBe('not registered');
		expect(issueLabel({ kind: 'pkgs_dir_duplicate', served_path: '/p/a' })).toBe('duplicate, not served');
	});

	const row = async (container: HTMLElement) =>
		waitFor(() => {
			const el = container.querySelector('[data-install="com.ikenga.meetings"]');
			if (!el) throw new Error('row not rendered yet');
			return el as HTMLElement;
		});

	it('shows Reinstall from registry AND Remove… AND Hand to Chi inline when the registry has it', async () => {
		const onReinstall = vi.fn();
		const { container } = mount({ canReinstall: (id) => id === 'com.ikenga.meetings', onReinstall });
		const r = await row(container);
		expect(r.querySelector('[data-issue="pkgs_dir_unloadable"]')?.textContent).toBe('failed to load');
		expect(r.textContent).toContain('ui.nav');
		const acts = r.querySelector('.acts') as HTMLElement;
		expect(within(acts).getByRole('button', { name: 'Reinstall from registry' })).toBeTruthy();
		expect(within(acts).getByRole('button', { name: 'Remove…' })).toBeTruthy();
		expect(within(acts).getByRole('button', { name: 'Hand to Chi' })).toBeTruthy();
		fireEvent.click(within(acts).getByRole('button', { name: 'Reinstall from registry' }));
		expect(onReinstall).toHaveBeenCalledWith('com.ikenga.meetings');
		fireEvent.click(within(acts).getByRole('button', { name: 'Hand to Chi' }));
		expect(vi.mocked(handToChi)).toHaveBeenCalledWith(expect.stringContaining('com.ikenga.meetings'));
	});

	it('shows Remove… (no Reinstall) when the registry does not list it', async () => {
		const { container } = mount({ canReinstall: () => false, onReinstall: vi.fn() });
		const r = await row(container);
		expect(r.querySelector('[data-reinstall]')).toBeNull();
		expect(r.querySelector('[data-remove="com.ikenga.meetings"]')).not.toBeNull();
	});

	it('Remove confirms first, then calls the retire path and says where the folder went', async () => {
		vi.mocked(cmd.pkgHealthRemove).mockResolvedValue({
			removed_rows: 0,
			retired: [
				{
					id: 'com.ikenga.meetings',
					path: MEETINGS.install_path,
					backup: 'C:/pkgs/.uninstalled-com.ikenga.meetings-1727700000000',
				},
			],
		});
		const { container } = mount({ canReinstall: () => true, onReinstall: vi.fn() });
		const r = await row(container);
		fireEvent.click(r.querySelector('[data-remove="com.ikenga.meetings"]') as HTMLElement);
		const dlg = await screen.findByRole('dialog');
		// Recoverable backup, not a delete; and it points at Reinstall.
		expect(dlg.textContent).toContain('.uninstalled-');
		expect(dlg.querySelector('[data-remove-recoverable]')?.textContent).toContain('7 days');
		expect(dlg.querySelector('[data-remove-reinstall-hint]')).not.toBeNull();
		expect(dlg.textContent).not.toMatch(/deletes its folder/);
		// Cancel ("Keep it") calls nothing.
		fireEvent.click(within(dlg).getByRole('button', { name: 'Keep it' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(cmd.pkgHealthRemove).not.toHaveBeenCalled();

		fireEvent.click(r.querySelector('[data-remove="com.ikenga.meetings"]') as HTMLElement);
		await act(async () => {
			fireEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Remove' }));
		});
		await waitFor(() => expect(cmd.pkgHealthRemove).toHaveBeenCalledWith('com.ikenga.meetings'));
		await waitFor(() =>
			expect(container.querySelector('[data-hnotice]')?.textContent).toContain(
				'moved its folder to C:/pkgs/.uninstalled-com.ikenga.meetings-1727700000000'
			)
		);
	});

	it('Remove all: the result line says exactly what happened, and never "done" while an issue remains', async () => {
		vi.mocked(cmd.pkgHealthRemoveAll).mockResolvedValue({
			removed_records: 0,
			removed_orphans: 0,
			retired_folders: [],
			failed: [{ id: 'com.ikenga.meetings', error: 'could not retire the folder: access denied' }],
			remaining: [MEETINGS],
			rescan_error: null,
		});
		const { container } = mount({ canReinstall: () => false });
		await row(container);
		fireEvent.click(container.querySelector('[data-act="remove-all"]') as HTMLElement);
		const dlg = await screen.findByRole('dialog');
		// Folder issues are covered, and the dialog says how.
		expect(dlg.querySelector('[data-removeall-folders]')?.textContent).toContain('com.ikenga.meetings');
		await act(async () => {
			fireEvent.click(within(dlg).getByRole('button', { name: 'Remove all' }));
		});
		await waitFor(() => expect(cmd.pkgHealthRemoveAll).toHaveBeenCalledTimes(1));
		const notice = await waitFor(() => {
			const n = container.querySelector('[data-hnotice]');
			if (!n) throw new Error('no notice');
			return n as HTMLElement;
		});
		expect(notice.textContent).toBe(
			'0 removed — 1 issue left: com.ikenga.meetings: could not retire the folder: access denied'
		);
		expect(notice.className).toContain('err');
		expect(notice.textContent).not.toContain('done');
		// The list reflects the kernel's rescan: the item is still shown.
		expect(container.querySelector('[data-install="com.ikenga.meetings"]')).not.toBeNull();
	});

	describe('in a remote web session (the headless daemon)', () => {
		afterEach(() => {
			vi.mocked(cmd.isRemoteWebSession).mockReturnValue(false);
		});

		it('Reinstall from registry is shown disabled with the desktop-only reason, like Remove', async () => {
			vi.mocked(cmd.isRemoteWebSession).mockReturnValue(true);
			const onReinstall = vi.fn();
			const { container } = mount({ canReinstall: () => true, onReinstall });
			const r = await row(container);
			const re = r.querySelector<HTMLButtonElement>('[data-reinstall="com.ikenga.meetings"]');
			expect(re).not.toBeNull();
			expect(re?.disabled).toBe(true);
			expect(re?.title).toBe('Desktop app only');
			fireEvent.click(re as HTMLElement);
			expect(onReinstall).not.toHaveBeenCalled();
			const rm = r.querySelector<HTMLButtonElement>('[data-remove="com.ikenga.meetings"]');
			expect(rm?.disabled).toBe(true);
			expect(rm?.title).toBe('Desktop app only');
		});
	});

	it('a duplicate pkgs-folder copy reads "duplicate, not served": no Reinstall, not "bad", not "disabled"', async () => {
		vi.mocked(cmd.pkgHealthScan).mockResolvedValueOnce([
			{
				id: 'com.ikenga.hello',
				install_path: '/pkgs/zz-dup',
				enabled: false,
				issue: { kind: 'pkgs_dir_duplicate', served_path: '/pkgs/hello' },
				detail: 'duplicate, not served: com.ikenga.hello is served from /pkgs/hello; this copy is ignored',
			},
		]);
		const onReinstall = vi.fn();
		const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
		const { container } = render(
			<QueryClientProvider client={qc}>
				<NgwaHealthSurface
					items={[]}
					snapshot={mkSnapshot([])}
					onOpenBackup={() => {}}
					onOpenStore={() => {}}
					canReinstall={() => true}
					onReinstall={onReinstall}
				/>
			</QueryClientProvider>
		);
		const r = await waitFor(() => {
			const el = container.querySelector('[data-install="com.ikenga.hello"]');
			if (!el) throw new Error('row not rendered yet');
			return el as HTMLElement;
		});
		const tag = r.querySelector('[data-issue="pkgs_dir_duplicate"]') as HTMLElement;
		expect(tag.textContent).toBe('duplicate, not served');
		expect(tag.className).not.toContain('bad');
		expect(r.querySelector('[data-reinstall]')).toBeNull();
		expect(r.textContent).not.toMatch(/\bdisabled\b/);
		expect(r.textContent).toContain('/pkgs/zz-dup');
	});
});

describe('Remove result lines', () => {
	const base = {
		removed_records: 0,
		removed_orphans: 0,
		retired_folders: [] as cmd.PkgHealthRetiredFolder[],
		failed: [] as Array<{ id: string; error: string }>,
		remaining: [] as cmd.PkgHealthIssue[],
		rescan_error: null as string | null,
	};
	const folder = { id: 'com.ikenga.meetings', path: '/p/m', backup: '/p/.uninstalled-m-1' };

	it('names every kind removed and is ok only when nothing is left', () => {
		expect(summarizeRemoveAll({ ...base, removed_records: 1, retired_folders: [folder] })).toEqual({
			tone: 'ok',
			text: 'Removed 1 record · retired 1 folder',
		});
		expect(
			summarizeRemoveAll({ ...base, removed_records: 2, removed_orphans: 3, retired_folders: [folder, folder] }).text
		).toBe('Removed 2 records · retired 2 folders · 3 orphan rows');
	});

	it('says what was left and why', () => {
		const left: cmd.PkgHealthIssue = {
			id: 'com.ikenga.meetings',
			install_path: '/p/m',
			enabled: false,
			issue: { kind: 'pkgs_dir_unloadable' },
			detail: 'x',
		};
		expect(summarizeRemoveAll({ ...base, remaining: [left] })).toEqual({
			tone: 'err',
			text: '0 removed — 1 issue left: com.ikenga.meetings needs Reinstall or Remove',
		});
		expect(summarizeRemoveAll({ ...base, removed_records: 1, rescan_error: 'db locked' })).toEqual({
			tone: 'err',
			text: 'Removed 1 record — the rescan failed, so what is left is unknown: db locked',
		});
		expect(summarizeRemoveAll({ ...base, failed: [{ id: 'orphan rows', error: 'busy' }] }).tone).toBe('err');
	});

	it('a single Remove names the rows deleted or the backup folder', () => {
		expect(summarizeRemove('a', { removed_rows: 3, retired: [] }).text).toBe('Removed a: deleted 3 record rows');
		expect(summarizeRemove('m', { removed_rows: 0, retired: [folder] }).text).toBe(
			'Removed m: moved its folder to /p/.uninstalled-m-1'
		);
	});
});

describe('D-02 layout', () => {
	const many: cmd.PkgHealthIssue[] = Array.from({ length: 40 }, (_, i) => ({
		id: `com.test.broken${i}`,
		install_path: `/pkgs/b${i}`,
		enabled: true,
		issue: { kind: 'manifest_missing' },
		detail: 'no manifest',
	}));

	function mountLayout() {
		vi.mocked(cmd.pkgHealthScan).mockResolvedValueOnce(many);
		const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
		const items = engineItems(['claude']);
		return render(
			<QueryClientProvider client={qc}>
				<NgwaHealthSurface items={items} snapshot={mkSnapshot(items)} onOpenBackup={() => {}} onOpenStore={() => {}} />
			</QueryClientProvider>
		);
	}

	it('renders six panels in D-02 order, with Trust as its own panel', () => {
		const { container } = mountLayout();
		const panels = [...container.querySelectorAll('[data-hgrid] > section.panel')].map((p) =>
			p.getAttribute('data-panel')
		);
		expect(panels).toEqual(['violations', 'sidecars', 'cron', 'data', 'trust', 'engines']);
		const trust = container.querySelector('[data-panel="trust"]') as HTMLElement;
		expect(trust.querySelector('h3')?.textContent).toContain('Trust');
		expect(trust.querySelector('[data-act="review-unsigned"]')).not.toBeNull();
		expect(trust.querySelector('[data-unsignedn]')).not.toBeNull();
		const violations = container.querySelector('[data-panel="violations"]') as HTMLElement;
		expect(violations.querySelector('[data-act="review-unsigned"]')).toBeNull();
		expect(violations.querySelector('[data-unsignedn]')).toBeNull();
		// No "Permission violations" / "Install records" subsections any more.
		expect(violations.querySelector('.hsub')).toBeNull();
		// The audit line closes the grid.
		expect(container.querySelector('[data-hgrid] > [data-auditline]')).not.toBeNull();
	});

	it('renders every row: nothing is capped, clipped or hidden', async () => {
		const { container } = mountLayout();
		await waitFor(() => expect(container.querySelectorAll('[data-install]').length).toBe(40));
		for (const r of container.querySelectorAll('[data-install]')) {
			expect(within(r as HTMLElement).getByRole('button', { name: 'Remove…' })).toBeTruthy();
		}
		// The page is the one scroller; the grid sits inside it.
		expect(container.querySelector('[data-hscroll] > [data-hgrid]')).not.toBeNull();
	});

	it('the stylesheet gives no health list a height cap and the grid no definite height', () => {
		const ngwaCss = readFileSync(join(dirname(fileURLToPath(import.meta.url)), 'ngwa.css'), 'utf8');
		const block = (sel: string) => {
			const m = ngwaCss.match(new RegExp(`${sel.replace(/[.]/g, '\\.')} \\{([^}]*)\\}`));
			return m?.[1] ?? '';
		};
		expect(block('.view-ngwa .hlist')).not.toMatch(/max-height/);
		const grid = block('.view-ngwa .hgrid');
		expect(grid).toMatch(/display: grid/);
		expect(grid).not.toMatch(/overflow|flex: 1|height/);
		expect(block('.view-ngwa .hscroll')).toMatch(/overflow-y: auto/);
	});
});

describe('engine probe when WSL could not be asked (D-10)', () => {
	it('says "WSL unavailable — <reason>", not "CLI not found"', async () => {
		const caps = { streaming: true, tool_use: true, thinking: false, artifacts: false, mcp: false, session_resume: false };
		vi.mocked(cmd.detectAgent).mockImplementation(async (id: string) =>
			id === 'claude-code'
				? {
						id,
						display: 'Claude Code',
						executable_path: 'claude (WSL)',
						version: null,
						authed: null,
						auth_hint: null,
						capabilities: caps,
						unavailable: { kind: 'wsl', reason: 'Wsl/Service/CreateInstance/E_FAIL' },
					}
				: null
		);
		const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
		const items = engineItems(['claude']);
		const { container } = render(
			<QueryClientProvider client={qc}>
				<NgwaHealthSurface items={items} snapshot={mkSnapshot(items)} onOpenBackup={() => {}} onOpenStore={() => {}} />
			</QueryClientProvider>
		);
		await waitFor(() =>
			expect(container.querySelector('[data-engine="claude"] [data-engine-probe]')?.textContent).toBe(
				"Couldn't check the CLI: WSL unavailable — Wsl/Service/CreateInstance/E_FAIL"
			)
		);
		await waitFor(() =>
			expect(container.querySelector('[data-engine="codex"] [data-engine-probe]')?.textContent).toBe(
				'CLI not found by the probe'
			)
		);
	});
});
