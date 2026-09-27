// WP-66 — a `chi` action targeting a seat (G-SEATS §9.1): resolved with
// `seats_resolve` (client `ui`, no takeover, no claim), with the runner's
// fail-closed guards on the route it returns.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const m = vi.hoisted(() => ({
	chiRun: vi.fn(),
	chiResume: vi.fn(),
	seatsResolve: vi.fn(),
	seatsQueue: vi.fn(),
	seatsResume: vi.fn(),
	resolveTarget: vi.fn(),
}));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	chiRun: m.chiRun,
	chiResume: m.chiResume,
	seatsResolve: m.seatsResolve,
	seatsQueue: m.seatsQueue,
	seatsResume: m.seatsResume,
}));

vi.mock('@/shell/companion/resolve-target', () => ({ resolveTarget: m.resolveTarget }));

import { useShellStore } from '@/lib/shell/shell-store';
import type { SeatView } from '@/lib/tauri-cmd';
import { useCompanionStore } from '@/shell/companion/companion-store';
import { useTerminalStore, type TerminalTab } from '@/terminal/session-store';
import {
	ChiUnavailableError,
	NON_CLAUDE_AGENT_REASON,
	PTY_SCOPE_REASON,
	seatAddressFor,
	send,
} from './chi';

function seat(over: Partial<SeatView> = {}): SeatView {
	return {
		id: 'seat-1',
		project_id: 'p1',
		name: 'docs',
		engine_id: 'claude-code',
		session: { kind: 'run', run_id: 'run-1', external_id: 'x', cwd: null },
		created_at: 0,
		last_active_at: 0,
		hold: null,
		address: 'seat:p1/docs',
		agent_id: 'seat-1',
		status: 'idle',
		agent: null,
		resume: { resumable: true },
		engine_resume: 'durable',
		mount: null,
		queued: null,
		pad: { count: 0, latest: null },
		inbox_count: 0,
		...over,
	};
}

/** A live Claude wrap tab (its `SessionStart` recorded). */
function agentTab(id: string): TerminalTab {
	return {
		id,
		title: id,
		spec: { cwd: '/proj', cmd: ['claude'], wrap: {} },
		claudeSessionId: `c-${id}`,
		agentLive: true,
		ptyId: `pty-${id}`,
		status: 'running',
		exitCode: null,
		createdAt: 0,
		owner: { kind: 'sidepane' },
	};
}

beforeEach(() => {
	useShellStore.setState({
		activeProject: { id: 'p1', root_path: '/proj', extra_roots: [] },
		companion: { activeTarget: { kind: 'new', engine_id: null } },
	});
	useCompanionStore.setState({ permissions: [] });
	useTerminalStore.setState({ tabs: [] });
	m.chiResume.mockResolvedValue({ run_id: 'run-1', status: 'running' });
});

afterEach(() => {
	vi.clearAllMocks();
});

describe('seat addressing', () => {
	it('qualifies a bare name with the action project, else the active one', () => {
		expect(seatAddressFor('docs')).toEqual({ address: 'p1/docs' });
		expect(seatAddressFor('docs', 'royalti-co')).toEqual({ address: 'royalti-co/docs' });
		expect(seatAddressFor('other/docs', 'royalti-co')).toEqual({ address: 'other/docs' });
	});

	it('resolves with client ui — never takeover, never a resume claim', async () => {
		m.seatsResolve.mockResolvedValue({ route: 'chi-resume', seat: seat(), run_id: 'run-1', busy: false });
		await send({ prompt: 'x', target: 'seat', seat: 'docs', scope: 'personal' });
		expect(m.seatsResolve).toHaveBeenCalledWith({ address: 'p1/docs' }, { client: 'ui' });
	});

	it('a project action resolves a bare name in its own project', async () => {
		m.seatsResolve.mockResolvedValue({ route: 'chi-resume', seat: seat(), run_id: 'run-1', busy: false });
		await send({ prompt: 'x', target: 'seat', seat: 'docs', projectId: 'proj-x', scope: 'project' });
		expect(m.seatsResolve).toHaveBeenCalledWith({ address: 'proj-x/docs' }, { client: 'ui' });
	});

	it('an `active` target on a seat resolves that seat by id', async () => {
		useShellStore.setState({ companion: { activeTarget: { kind: 'seat', seat_id: 'seat-1' } } });
		m.seatsResolve.mockResolvedValue({ route: 'chi-resume', seat: seat(), run_id: 'run-1', busy: false });
		await send({ prompt: 'x', target: 'active', scope: 'personal' });
		expect(m.seatsResolve).toHaveBeenCalledWith({ seatId: 'seat-1' }, { client: 'ui' });
		expect(m.resolveTarget).not.toHaveBeenCalled();
	});

	it('target "seat" with no seat name is refused: no-seat', async () => {
		await expect(send({ prompt: 'x', target: 'seat', scope: 'personal' })).rejects.toMatchObject({
			reason: 'no-seat',
		});
		expect(m.seatsResolve).not.toHaveBeenCalled();
	});
});

describe('routes', () => {
	it('chi-resume → chiResume, returning the run id', async () => {
		m.seatsResolve.mockResolvedValue({ route: 'chi-resume', seat: seat(), run_id: 'run-1', busy: false });
		await expect(send({ prompt: 'go', target: 'seat', seat: 'docs', scope: 'project' })).resolves.toEqual({
			runId: 'run-1',
			via: 'chi-resume',
		});
		expect(m.chiResume).toHaveBeenCalledWith('run-1', 'go');
	});

	it('a busy run queues (§4.5) — never chi_resume', async () => {
		m.seatsResolve.mockResolvedValue({ route: 'chi-resume', seat: seat({ status: 'run' }), run_id: 'run-1', busy: true });
		m.seatsQueue.mockResolvedValue(seat());
		await expect(send({ prompt: 'go', target: 'seat', seat: 'docs', scope: 'package' })).resolves.toEqual({
			runId: 'run-1',
			via: 'chi-resume',
		});
		expect(m.seatsQueue).toHaveBeenCalledWith('seat-1', 'go', { client: 'ui' });
		expect(m.chiResume).not.toHaveBeenCalled();
	});

	it('a vacant seat resumes headless on path H (project actions may use it)', async () => {
		m.seatsResolve.mockResolvedValue({ route: 'vacant', seat: seat({ status: 'vacant' }), resume: { resumable: true }, claim: null });
		m.seatsResume.mockResolvedValue({ seat: seat(), run_id: 'run-2', outcome: 'resumed', previous: null });
		await expect(send({ prompt: 'go', target: 'seat', seat: 'docs', scope: 'project' })).resolves.toEqual({
			runId: 'run-2',
			via: 'chi-resume',
		});
		expect(m.seatsResume).toHaveBeenCalledWith('seat-1', 'go', { client: 'ui' }, { fallback: 'fresh' });
	});

	it('a fresh start reports via chi-run', async () => {
		m.seatsResolve.mockResolvedValue({ route: 'vacant', seat: seat({ status: 'vacant' }), resume: { resumable: false, reason: 'no_session' }, claim: null });
		m.seatsResume.mockResolvedValue({ seat: seat(), run_id: 'run-3', outcome: 'started-fresh', previous: null, reason: 'no_session' });
		await expect(send({ prompt: 'go', target: 'seat', seat: 'docs', scope: 'personal' })).resolves.toEqual({
			runId: 'run-3',
			via: 'chi-run',
		});
	});
});

describe('the PTY route keeps every DEC-55 guard', () => {
	const ptyRoute = (agent: 'live' | 'unreported') => ({
		route: 'pty',
		seat: seat({ status: 'live', agent }),
		terminal_id: 's1',
		agent,
		lease_holder: null,
	});

	it('a personal action injects into a live Claude agent (no run id)', async () => {
		const ptySend = vi.fn().mockResolvedValue(undefined);
		useTerminalStore.setState({ tabs: [agentTab('s1')] });
		m.seatsResolve.mockResolvedValue(ptyRoute('live'));
		m.resolveTarget.mockReturnValue({ kind: 'pty', send: ptySend });
		await expect(send({ prompt: 'ls', target: 'seat', seat: 'docs', scope: 'personal' })).resolves.toEqual({
			runId: null,
			via: 'pty',
		});
		expect(m.resolveTarget).toHaveBeenCalledWith({ kind: 'session', session_id: 's1' });
		expect(ptySend).toHaveBeenCalledWith('ls');
	});

	it('a project action never types into a terminal', async () => {
		useTerminalStore.setState({ tabs: [agentTab('s1')] });
		m.seatsResolve.mockResolvedValue(ptyRoute('live'));
		await expect(send({ prompt: 'ls', target: 'seat', seat: 'docs', scope: 'project' })).rejects.toMatchObject({
			reason: 'no-target',
			message: PTY_SCOPE_REASON,
		});
		expect(m.resolveTarget).not.toHaveBeenCalled();
	});

	it("an `agent: 'unreported'` route is refused", async () => {
		useTerminalStore.setState({ tabs: [agentTab('s1')] });
		m.seatsResolve.mockResolvedValue(ptyRoute('unreported'));
		await expect(send({ prompt: 'ls', target: 'seat', seat: 'docs', scope: 'personal' })).rejects.toMatchObject({
			reason: 'no-target',
			message: NON_CLAUDE_AGENT_REASON,
		});
	});

	it('a pending permission refuses the inject', async () => {
		useTerminalStore.setState({ tabs: [{ ...agentTab('s1'), permissionPending: true }] });
		m.seatsResolve.mockResolvedValue(ptyRoute('live'));
		await expect(send({ prompt: 'ls', target: 'seat', seat: 'docs', scope: 'personal' })).rejects.toMatchObject({
			reason: 'permission-pending',
		});
	});

	it('a `!` line is refused', async () => {
		useTerminalStore.setState({ tabs: [agentTab('s1')] });
		m.seatsResolve.mockResolvedValue(ptyRoute('live'));
		await expect(send({ prompt: '!rm -rf ~', target: 'seat', seat: 'docs', scope: 'personal' })).rejects.toMatchObject({
			reason: 'bang-prompt',
		});
	});
});

describe('typed refusals (§9.1)', () => {
	it.each([
		['seat_not_found', 'no-seat'],
		['invalid_address', 'no-seat'],
		['seat_held', 'seat-held'],
		['seat_taken_over', 'seat-held'],
		['agent_not_live', 'no-target'],
	])('%s → %s', async (code, reason) => {
		m.seatsResolve.mockRejectedValueOnce({ code, message: code });
		const err = await send({ prompt: 'x', target: 'seat', seat: 'docs', scope: 'personal' }).catch((e) => e);
		expect(err).toBeInstanceOf(ChiUnavailableError);
		expect(err.reason).toBe(reason);
	});

	it('a seat_not_vacant race resolves once more', async () => {
		m.seatsResolve
			.mockResolvedValueOnce({ route: 'vacant', seat: seat(), resume: { resumable: true }, claim: null })
			.mockResolvedValueOnce({ route: 'chi-resume', seat: seat(), run_id: 'run-1', busy: false });
		m.seatsResume.mockRejectedValueOnce({ code: 'seat_not_vacant', message: 'no' });
		await expect(send({ prompt: 'x', target: 'seat', seat: 'docs', scope: 'personal' })).resolves.toEqual({
			runId: 'run-1',
			via: 'chi-resume',
		});
		expect(m.seatsResolve).toHaveBeenCalledTimes(2);
	});
});
