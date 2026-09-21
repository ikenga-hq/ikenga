// /ngwa/health route tests (WP-16a). Real route in a real router, `tauri-cmd`
// mocked: nothing touches the real database, ~/.claude or a pkg install.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, screen, waitFor, within } from '@testing-library/react';
import * as cmd from '@/lib/tauri-cmd';
import type { NgwaSnapshot } from '@ikenga/contract';
import { Route as HealthRoute } from './health';
import { engineItems, mkItem, mkSnapshot, mountRoutes } from './-ngwa-test-fixtures';

vi.mock('@/lib/registry/use-registry', () => ({
	useRegistryIndex: () => ({ data: undefined, isLoading: false, error: null }),
}));

vi.mock('@/lib/tauri-cmd', async (orig) => {
	const actual = await orig<typeof import('@/lib/tauri-cmd')>();
	return {
		...actual,
		ngwaSnapshot: vi.fn(),
		pkgPermissionViolationsList: vi.fn(),
		pkgPermissionViolationsClear: vi.fn(),
		pkgHealthScan: vi.fn(),
		pkgHealthRemove: vi.fn(),
		pkgHealthRemoveAll: vi.fn(),
		pkgKernelStatus: vi.fn(),
		pkgSupervisorRestart: vi.fn(),
		agentOpsListJobs: vi.fn(),
		agentOpsRunNow: vi.fn(),
		agentOpsTailRun: vi.fn(),
		dataHealthScan: vi.fn(),
		dataHealthDbSize: vi.fn(),
		backupList: vi.fn(),
		detectAgent: vi.fn(),
	};
});

const m = vi.mocked(cmd);

const VIOLATIONS = [1, 2, 3, 4, 5].map((i) => ({
	id: i,
	pkg_id: i < 4 ? 'pkg.alpha' : 'pkg.beta',
	scope_kind: i % 2 ? 'http' : 'shell.execute',
	attempted: `https://x/${i}`,
	declared: i === 5 ? '' : 'https://y',
	occurred_at: Date.UTC(2026, 8, 20, 10, i),
}));

const INSTALLS: cmd.PkgHealthIssue[] = [
	{ id: 'pkg.broken1', install_path: '/pkgs/b1', enabled: true, issue: { kind: 'manifest_missing' }, detail: 'no manifest' },
	{ id: 'pkg.broken2', install_path: '/pkgs/b2', enabled: false, issue: { kind: 'manifest_unparseable' }, detail: 'bad json' },
	{ id: 'pkg.broken3', install_path: '/pkgs/b3', enabled: true, issue: { kind: 'api_incompatible', ikenga_api: '9' }, detail: 'api 9' },
	{ id: 'pkg.gone', install_path: '', enabled: true, issue: { kind: 'orphan_row', table: 'pkg_settings' }, detail: '2 rows' },
];

function kernel(over: Record<string, unknown> = {}): cmd.PkgKernelStatus {
	return {
		installed: [],
		api_version: 1,
		registries: {
			sidecar_supervisor: {
				count: 2,
				entries: [
					{ pkg_id: 'com.ikenga.git', state: 'crashed', pid: null, uptime_s: null, restarts: 2, last_crash_unix_ms: Date.UTC(2026, 8, 21), last_err: 'exit 1' },
					{ pkg_id: 'com.ikenga.studio', state: 'running', pid: 4242, uptime_s: 30, restarts: 0, last_crash_unix_ms: null, last_err: null },
				],
			},
			cron: { count: 1, entries: [{ pkg_id: 'com.ikenga.mail', cron_id: 'poll', expr: '0 */5 * * * *', handler: 'sidecar:mail poll', job_uuid: 'u' }] },
			...over,
		},
	};
}

const JOBS = {
	ok: true as const,
	daemon_up: true,
	daemon_pid: 777,
	jobs: [
		{
			id: 'pulse-refresh', label: 'pulse-refresh', schedule: '0 5 * * *', schedule_dialect: '5f' as const,
			timezone: 'UTC', enabled: true, command: 'x', mode: 'script' as const, model: null, agent: null,
			_disabledReason: null,
			state: { nextRunAtMs: Date.UTC(2026, 8, 22, 5), lastRunAtMs: Date.UTC(2026, 8, 21, 5), lastStatus: 'ok', consecutiveErrors: 0, lastDurationMs: 1, totalCostUsd: null, totalRuns: 3, lastUsage: null },
		},
		{
			id: 'never-ran', label: '', schedule: '0 6 * * *', schedule_dialect: '5f' as const,
			timezone: 'UTC', enabled: true, command: 'y', mode: 'agent' as const, model: null, agent: null,
			_disabledReason: null, state: null,
		},
	],
};

function healthItems() {
	return [
		...engineItems(['claude', 'codex']),
		// unsigned: signed false, not awaiting approval
		mkItem({ id: 'skill:personal:a', kind: 'skill', name: 'a' }),
		mkItem({
			id: 'com.x.app', kind: 'app', name: 'com.x.app',
			trust: { state: 'granted', signed: false, auto_trusted: false, review_pending: false, perms: null, last_granted_at_ms: null },
		}),
		// not unsigned: awaiting approval
		mkItem({
			id: 'com.x.review', kind: 'app', name: 'com.x.review',
			trust: { state: 'needs_approval', signed: false, auto_trusted: false, review_pending: true, perms: null, last_granted_at_ms: null },
		}),
	];
}

beforeEach(() => {
	m.ngwaSnapshot.mockResolvedValue(mkSnapshot(healthItems()));
	m.pkgPermissionViolationsList.mockResolvedValue(VIOLATIONS);
	m.pkgPermissionViolationsClear.mockResolvedValue(3);
	m.pkgHealthScan.mockResolvedValue(INSTALLS);
	m.pkgHealthRemove.mockResolvedValue(undefined);
	m.pkgHealthRemoveAll.mockResolvedValue({ removed_records: 3, removed_orphans: 1 });
	m.pkgKernelStatus.mockResolvedValue(kernel());
	m.pkgSupervisorRestart.mockResolvedValue(true);
	m.agentOpsListJobs.mockResolvedValue(JOBS);
	m.agentOpsRunNow.mockResolvedValue({ ok: true, status: 202, message: 'queued' });
	m.agentOpsTailRun.mockResolvedValue({
		ok: true, running: false, status: 'done', startedAtMs: 1, mode: 'script', chunk: 'hello from the run', nextOffset: 18, eof: true,
	});
	m.dataHealthScan.mockResolvedValue([
		{ table: 'tasks', column: 'initiative_id', parent_table: 'strategic_initiatives', orphan_count: 7, sample_ids: ['t1', 't2'] },
	]);
	m.dataHealthDbSize.mockResolvedValue({ db_path: '/data/pa.db', db_bytes: 5 * 1024 * 1024, wal_bytes: 2048, shm_bytes: null });
	m.backupList.mockResolvedValue([
		{ path: '/b/old.ikbak', created_at: '2026-09-01T00:00:00Z', size_bytes: 1, schema_version: 1, has_secrets: false, pkg_count: 1, path_mode: 'raw' },
		{ path: '/b/new.ikbak', created_at: '2026-09-20T00:00:00Z', size_bytes: 1, schema_version: 1, has_secrets: false, pkg_count: 1, path_mode: 'raw' },
	] as cmd.BackupSummary[]);
	m.detectAgent.mockImplementation(async (id: string) =>
		id === 'claude-code'
			? {
					id, display: 'Claude Code', executable_path: '/usr/bin/claude', version: '2.0.1', authed: true, auth_hint: null,
					capabilities: { streaming: true, tool_use: true, thinking: true, artifacts: true, mcp: true, session_resume: true },
				}
			: null
	);
});

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

function mount(url = '/ngwa/health') {
	return mountRoutes(
		[
			{ route: HealthRoute, path: '/ngwa/health' },
		],
		url
	);
}

const panel = (s: string) => document.querySelector<HTMLElement>(`[data-panel="${s}"]`) as HTMLElement;
const dialog = () => screen.getByRole('dialog');

describe('/ngwa/health — Violations panel (fold-in of pkg-audit + pkg-health)', () => {
	it('lists every violation with scope label and timestamp, not a preview', async () => {
		mount();
		await waitFor(() => expect(document.querySelectorAll('[data-violation]').length).toBe(5));
		const txt = panel('violations').textContent ?? '';
		expect(txt).toContain('host.fetch');
		expect(txt).toContain('spawn');
		expect(txt).toContain('2026-09-20 10:01:00');
	});

	it('lists every install record with its issue badge; an orphan row is not "broken"', async () => {
		mount();
		await waitFor(() => expect(document.querySelectorAll('[data-install]').length).toBe(4));
		const orphan = document.querySelector('[data-install="pkg.gone"] [data-issue]');
		expect(orphan?.textContent).toBe('orphan: pkg_settings');
		expect(orphan?.className).not.toContain('bad');
		expect(document.querySelector('[data-install="pkg.broken3"] [data-issue]')?.textContent).toBe('api 9');
	});

	it('renders independently of a snapshot that is still loading, with the 1–2 minute copy', async () => {
		m.ngwaSnapshot.mockReturnValue(new Promise(() => {}));
		mount();
		await waitFor(() => expect(document.querySelectorAll('[data-violation]').length).toBe(5));
		expect(document.querySelectorAll('[data-install]').length).toBe(4);
		expect(document.querySelector('[data-snapshot-loading]')?.textContent).toContain('1–2 minutes');
		expect(document.querySelector('[data-unsignedn]')?.textContent).toContain('—');
	});

	it('Clear: cancel calls nothing; confirm calls pkgPermissionViolationsClear(pkg)', async () => {
		mount();
		const btn = await waitFor(() => {
			const b = document.querySelector<HTMLButtonElement>('[data-clear="pkg.alpha"]');
			if (!b) throw new Error('no clear');
			return b;
		});
		fireEvent.click(btn);
		expect(dialog().textContent).toContain('3 audit rows');
		fireEvent.click(within(dialog()).getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(m.pkgPermissionViolationsClear).not.toHaveBeenCalled();
		fireEvent.click(btn);
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Clear' }));
		});
		await waitFor(() => expect(m.pkgPermissionViolationsClear).toHaveBeenCalledTimes(1));
		expect(m.pkgPermissionViolationsClear).toHaveBeenCalledWith('pkg.alpha');
	});

	it('Remove: cancel calls nothing; confirm calls pkgHealthRemove(id)', async () => {
		mount();
		const btn = await waitFor(() => {
			const b = document.querySelector<HTMLButtonElement>('[data-remove="pkg.broken1"]');
			if (!b) throw new Error('no remove');
			return b;
		});
		fireEvent.click(btn);
		expect(dialog().textContent).toContain('/pkgs/b1');
		fireEvent.click(within(dialog()).getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(m.pkgHealthRemove).not.toHaveBeenCalled();
		fireEvent.click(btn);
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Remove' }));
		});
		await waitFor(() => expect(m.pkgHealthRemove).toHaveBeenCalledTimes(1));
		expect(m.pkgHealthRemove).toHaveBeenCalledWith('pkg.broken1');
	});

	it('Remove all: cancel calls nothing; confirm calls pkgHealthRemoveAll()', async () => {
		mount();
		const btn = await waitFor(() => {
			const b = document.querySelector<HTMLButtonElement>('[data-act="remove-all"]');
			if (!b) throw new Error('no remove all');
			return b;
		});
		fireEvent.click(btn);
		fireEvent.click(within(dialog()).getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(m.pkgHealthRemoveAll).not.toHaveBeenCalled();
		fireEvent.click(btn);
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Remove all' }));
		});
		await waitFor(() => expect(m.pkgHealthRemoveAll).toHaveBeenCalledTimes(1));
	});

	it('Refresh refetches both lists', async () => {
		mount();
		await waitFor(() => expect(document.querySelectorAll('[data-violation]').length).toBe(5));
		fireEvent.click(screen.getByRole('button', { name: 'Refresh violations' }));
		fireEvent.click(screen.getByRole('button', { name: 'Rescan install records' }));
		await waitFor(() => expect(m.pkgPermissionViolationsList).toHaveBeenCalledTimes(2));
		await waitFor(() => expect(m.pkgHealthScan).toHaveBeenCalledTimes(2));
	});

	it('counts unsigned per gate §5 and lists them on Review', async () => {
		mount();
		await waitFor(() =>
			expect(document.querySelector('[data-unsigned]')?.textContent).toBe('3 of 5 installed items are unsigned')
		);
		expect(panel('violations').textContent).not.toContain('came from unsigned manifests');
		fireEvent.click(document.querySelector('[data-act="review-unsigned"]') as HTMLElement);
		const list = document.querySelector('[data-list="unsigned"]');
		expect(list?.textContent).toContain('com.x.app');
		expect(list?.textContent).not.toContain('com.x.review');
	});

	it('an unreadable trust source makes the unsigned count unknown, not a number', async () => {
		const snap = mkSnapshot(healthItems());
		snap.sources.trust = { ok: false, error: 'db locked', count: 0 };
		m.ngwaSnapshot.mockResolvedValue(snap);
		mount();
		await waitFor(() => expect(document.querySelector('[data-unsigned-unknown]')).not.toBeNull());
		expect(document.querySelector('[data-unsigned]')).toBeNull();
	});

	it('no revoked-keys claim is rendered', async () => {
		mount();
		await waitFor(() => expect(document.querySelectorAll('[data-violation]').length).toBe(5));
		expect(document.body.textContent).not.toMatch(/revoked/i);
	});
});

describe('/ngwa/health — errors are errors', () => {
	it('violations and install failures render errors, never "none"', async () => {
		m.pkgPermissionViolationsList.mockRejectedValue(new Error('audit table gone'));
		m.pkgHealthScan.mockRejectedValue(new Error('kernel busy'));
		mount();
		await waitFor(() => expect(panel('violations').textContent).toContain('audit table gone'));
		expect(panel('violations').textContent).toContain('kernel busy');
		expect(document.querySelector('[data-empty="violations"]')).toBeNull();
		expect(document.querySelector('[data-empty="installs"]')).toBeNull();
		expect(document.querySelector('[data-violn]')).toBeNull();
	});

	it('kernel status failure is an error in Sidecars and pkg cron', async () => {
		m.pkgKernelStatus.mockRejectedValue(new Error('no kernel'));
		mount();
		await waitFor(() => expect(panel('sidecars').textContent).toContain('no kernel'));
		expect(panel('cron').textContent).toContain('no kernel');
		expect(document.querySelector('[data-sidecarn]')?.textContent).toBe('—');
	});

	it('a malformed supervisor snapshot is an error, not zero sidecars', async () => {
		m.pkgKernelStatus.mockResolvedValue(kernel({ sidecar_supervisor: { count: 0 } }));
		mount();
		await waitFor(() => expect(panel('sidecars').textContent).toContain('no entries array'));
	});

	it('agent-ops ok:false is an error', async () => {
		m.agentOpsListJobs.mockResolvedValue({ ok: false, code: 'io_error', status: null, error: 'read config: ENOENT' });
		mount();
		await waitFor(() => expect(panel('cron').textContent).toContain('io_error: read config: ENOENT'));
	});

	it('backup, measure and scan failures render errors', async () => {
		m.backupList.mockRejectedValue(new Error('backups dir unreadable'));
		m.dataHealthDbSize.mockRejectedValue(new Error('stat denied'));
		m.dataHealthScan.mockRejectedValue(new Error('reader pool down'));
		mount();
		await waitFor(() => expect(panel('data').textContent).toContain('backups dir unreadable'));
		fireEvent.click(document.querySelector('[data-act="measure"]') as HTMLElement);
		await waitFor(() => expect(panel('data').textContent).toContain('stat denied'));
		fireEvent.click(document.querySelector('[data-act="scan"]') as HTMLElement);
		await waitFor(() => expect(panel('data').textContent).toContain('reader pool down'));
	});
});

describe('/ngwa/health — Sidecars, Cron, Data, Engines use real sources', () => {
	it('Sidecars come from the supervisor; Restart calls pkgSupervisorRestart; Logs is disabled with a reason', async () => {
		mount();
		const btn = await waitFor(() => {
			const b = document.querySelector<HTMLButtonElement>('[data-restart="com.ikenga.git"]');
			if (!b) throw new Error('no restart');
			return b;
		});
		expect(document.querySelector('[data-restart="com.ikenga.studio"]')).toBeNull(); // running
		expect(panel('sidecars').textContent).toContain('2 restarts');
		fireEvent.click(btn);
		await waitFor(() => expect(m.pkgSupervisorRestart).toHaveBeenCalledWith('com.ikenga.git'));
		const logs = within(panel('sidecars')).getAllByRole('button', { name: 'Logs' })[0] as HTMLButtonElement;
		expect(logs.disabled).toBe(true);
		expect(logs.title).toContain('No sidecar log source');
	});

	it('Cron lists agent-ops jobs (Run now, Logs) and pkg cron (last run —, no Run now)', async () => {
		mount();
		await waitFor(() => expect(document.querySelector('[data-job="pulse-refresh"]')).not.toBeNull());
		expect(document.querySelector('[data-daemon]')?.textContent).toContain('daemon running (pid 777)');
		expect(document.querySelector('[data-job="never-ran"]')?.textContent).toContain('last run —');
		const pc = document.querySelector('[data-pkgcron="com.ikenga.mail::poll"]') as HTMLElement;
		expect(pc.textContent).toContain('last run —');
		expect(within(pc).queryByRole('button')).toBeNull();

		fireEvent.click(document.querySelector('[data-runnow="pulse-refresh"]') as HTMLElement);
		await waitFor(() => expect(m.agentOpsRunNow).toHaveBeenCalledWith('pulse-refresh'));
		fireEvent.click(document.querySelector('[data-logs="pulse-refresh"]') as HTMLElement);
		await waitFor(() => expect(m.agentOpsTailRun).toHaveBeenCalledWith('pulse-refresh', 0));
		await waitFor(() =>
			expect(document.querySelector('[data-log="pulse-refresh"]')?.textContent).toBe('hello from the run')
		);
	});

	it('Run now is disabled with a reason while the daemon is down', async () => {
		m.agentOpsListJobs.mockResolvedValue({ ...JOBS, daemon_up: false, daemon_pid: null });
		mount();
		const b = await waitFor(() => {
			const x = document.querySelector<HTMLButtonElement>('[data-runnow="pulse-refresh"]');
			if (!x) throw new Error('none');
			return x;
		});
		expect(b.disabled).toBe(true);
		expect(b.title).toContain('not running');
	});

	it('Measure calls the DEC-32 command and shows its measured sizes', async () => {
		mount();
		await waitFor(() => expect(document.querySelector('[data-slot="dbsize"]')?.textContent).toBe('—'));
		fireEvent.click(document.querySelector('[data-act="measure"]') as HTMLElement);
		await waitFor(() => expect(document.querySelector('[data-slot="dbsize"]')?.textContent).toBe('5.0 MB'));
		expect(m.dataHealthDbSize).toHaveBeenCalledTimes(1);
		expect(panel('data').textContent).toContain('/data/pa.db');
		expect(panel('data').textContent).toContain('shm absent');
	});

	it('Scan runs the orphan scan once and shows the full per-link table', async () => {
		mount();
		const scan = await waitFor(() => {
			const b = document.querySelector<HTMLElement>('[data-act="scan"]');
			if (!b) throw new Error('no scan');
			return b;
		});
		fireEvent.click(scan);
		await waitFor(() => expect(document.querySelector('[data-orphan="tasks.initiative_id"]')).not.toBeNull());
		expect(m.dataHealthScan).toHaveBeenCalledTimes(1);
		const row = document.querySelector('[data-orphan="tasks.initiative_id"]')?.textContent ?? '';
		expect(row).toContain('strategic_initiatives.id');
		expect(row).toContain('t1');
		expect(row).toContain('+5 more');
	});

	it('Last backup is the newest from backupList; Back up now navigates to /settings/backup', async () => {
		const { router } = mount();
		await waitFor(() =>
			expect(document.querySelector('[data-slot="backup"]')?.textContent).toBe('2026-09-20T00:00:00Z')
		);
		expect(panel('data').textContent).toContain('/b/new.ikbak');
		fireEvent.click(document.querySelector('[data-act="backup"]') as HTMLElement);
		await waitFor(() => expect(router.state.location.pathname).toBe('/settings/backup'));
	});

	it('Engines: installed from the shared signal, CLI facts only from the probe', async () => {
		mount();
		await waitFor(() => expect(document.querySelector('[data-enginen]')?.textContent).toBe('2 of 3'));
		const claude = document.querySelector('[data-engine="claude"]')?.textContent ?? '';
		expect(claude).toContain('2.0.1');
		expect(claude).toContain('CLI at /usr/bin/claude');
		const codex = document.querySelector('[data-engine="codex"]')?.textContent ?? '';
		expect(codex).toContain('engine pkg com.ikenga.engine-codex');
		expect(codex).toContain('CLI not found by the probe');
		expect(document.body.textContent).not.toContain('found on PATH');
		const gemini = document.querySelector('[data-engine="gemini"]') as HTMLElement;
		expect(gemini.textContent).toContain('No engine pkg installed');
		expect(within(gemini).getByRole('button', { name: 'Open Store' })).toBeTruthy();
	});

	it('Open Store (Engines) navigates to the Store filtered to engines', async () => {
		const { router } = mount();
		const gemini = await waitFor(() => {
			const g = document.querySelector<HTMLElement>('[data-engine="gemini"]');
			if (!g || !within(g).queryByRole('button', { name: 'Open Store' })) throw new Error('not yet');
			return g;
		});
		fireEvent.click(within(gemini).getByRole('button', { name: 'Open Store' }));
		await waitFor(() => expect(router.state.location.pathname).toBe('/ngwa/store'));
		expect(router.state.location.search).toEqual({ kind: 'engine' });
	});

	it('a probe failure is an error, not "not found"', async () => {
		m.detectAgent.mockRejectedValue(new Error('spawn EPERM'));
		mount();
		await waitFor(() =>
			expect(document.querySelector('[data-engine="codex"]')?.textContent).toContain('CLI probe failed: spawn EPERM')
		);
	});
});

describe('/ngwa/health — auditline and ?section=', () => {
	it('says how old the snapshot is and lists only the sources actually read', async () => {
		const snap: NgwaSnapshot = mkSnapshot(healthItems(), { as_of_ms: Date.now() - 5 * 60_000 - 1000 });
		snap.sources.usage = { ok: false, error: 'no corpus', count: 0 };
		m.ngwaSnapshot.mockResolvedValue(snap);
		m.backupList.mockRejectedValue(new Error('nope'));
		mount();
		const line = await waitFor(() => {
			const t = document.querySelector('[data-auditline]')?.textContent ?? '';
			if (!t.includes('Audited 5 min ago')) throw new Error(t);
			return t;
		});
		await waitFor(() =>
			expect(document.querySelector('[data-auditline]')?.textContent).toContain('agent-ops')
		);
		const final = document.querySelector('[data-auditline]')?.textContent ?? line;
		expect(final).toContain('kernel');
		// usage is not listed among the sources read; it is named as unreadable.
		const read = final.slice(0, final.indexOf('could not read'));
		expect(read).not.toContain('usage');
		expect(final).toContain('could not read: usage');
		expect(read).not.toContain('backups folder'); // failed: listed as unreadable instead
		expect(final).toContain('could not read: usage, backups folder');
		expect(final).not.toContain('SQLite file sizes'); // not measured yet
	});

	it('the auditline shows a healthy check only when every source read cleanly', async () => {
		mount();
		await waitFor(() =>
			expect(document.querySelector('[data-auditline]')?.getAttribute('data-audit-state')).toBe('ok')
		);
		cleanup();
		m.backupList.mockRejectedValue(new Error('nope'));
		mount();
		await waitFor(() =>
			expect(document.querySelector('[data-auditline]')?.textContent).toContain('could not read: backups folder')
		);
		expect(document.querySelector('[data-auditline]')?.textContent).toContain('Audited');
		expect(document.querySelector('[data-auditline]')?.getAttribute('data-audit-state')).toBe('degraded');
	});

	it('a failed snapshot reads as an error in the auditline, not "not read yet"', async () => {
		m.ngwaSnapshot.mockRejectedValue(new Error('scan exploded'));
		mount();
		await waitFor(() =>
			expect(document.querySelector('[data-auditline]')?.textContent).toContain('Snapshot failed: scan exploded')
		);
		expect(document.querySelector('[data-auditline]')?.textContent).not.toContain('not read yet');
		expect(document.querySelector('[data-auditline]')?.getAttribute('data-audit-state')).toBe('degraded');
	});

	it('?section=data focuses the Data panel', async () => {
		mount('/ngwa/health?section=data');
		await waitFor(() => expect(document.activeElement?.getAttribute('data-panel')).toBe('data'));
	});
});
