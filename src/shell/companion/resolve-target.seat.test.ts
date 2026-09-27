// WP-66 — a `seat` dispatch target (G-SEATS §9.4, Round 47 E-1/E-4):
// labels from the `seats_list` cache, routes at send time through
// `seats_resolve`, and shows the §6.3 / §4.5 / §5.2 notices.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { SeatRoute, SeatView } from '@/lib/tauri-cmd';

const m = vi.hoisted(() => ({
	ptyWrite: vi.fn(async () => {}),
	chiRun: vi.fn(async () => ({ run_id: 'run-new', status: 'queued' })),
	chiResume: vi.fn(async () => ({ run_id: 'run-1', status: 'running' })),
	seatsResolve: vi.fn(),
	seatsMove: vi.fn(),
	seatsQueue: vi.fn(),
	seatsResume: vi.fn(),
	cachedSeat: vi.fn(),
	cachedSeats: vi.fn(),
	prefetchSeats: vi.fn(),
	invalidateSeats: vi.fn(async () => {}),
	ensureSeatsLiveSync: vi.fn(),
	openTabPty: vi.fn(async () => ({})),
	buildAgentWrappedCmd: vi.fn(() => ['/bin/bash', '-i', '-c', 'agent']),
}));

vi.mock('@/lib/tauri-cmd', () => ({
	ptyWrite: m.ptyWrite,
	chiRun: m.chiRun,
	chiResume: m.chiResume,
	seatsResolve: m.seatsResolve,
	seatsMove: m.seatsMove,
	seatsQueue: m.seatsQueue,
	seatsResume: m.seatsResume,
}));

vi.mock('@/lib/queries/seats', async (orig) => ({
	// The real `seatErrorOf` / `UI_SEAT_CLIENT`; the cache reads are stubbed.
	...(await orig<typeof import('@/lib/queries/seats')>()),
	cachedSeat: m.cachedSeat,
	cachedSeats: m.cachedSeats,
	prefetchSeats: m.prefetchSeats,
	invalidateSeats: m.invalidateSeats,
	ensureSeatsLiveSync: m.ensureSeatsLiveSync,
	onSeatsChanged: vi.fn(() => () => {}),
}));

const ptys = new Map<string, { id: string; mode: 'ephemeral' | 'persistent'; exited: boolean }>();
vi.mock('@/terminal/pty-registry', () => ({ getPty: (id: string) => ptys.get(id) }));

type Tab = { id: string; spec: Record<string, unknown>; status: string; claudeSessionId?: string | null };
const terminal = vi.hoisted(() => ({ tabs: [] as Tab[] }));
vi.mock('@/terminal/session-store', () => ({
	makeTerminalId: () => 'term-new-0001',
	openTabPty: m.openTabPty,
	useTerminalStore: {
		getState: () => ({
			tabs: terminal.tabs,
			add: (spec: Record<string, unknown>, _title: string, id: string) => {
				terminal.tabs.push({ id, spec, status: 'spawning' });
				return id;
			},
			remove: (id: string) => {
				terminal.tabs = terminal.tabs.filter((t) => t.id !== id);
			},
		}),
		setState: (fn: (s: { tabs: Tab[] }) => { tabs: Tab[] }) => {
			terminal.tabs = fn({ tabs: terminal.tabs }).tabs;
		},
	},
}));

vi.mock('@/terminal/claude-wrap', () => ({ buildAgentWrappedCmd: m.buildAgentWrappedCmd }));
vi.mock('@/lib/shell/active-project-cwd', () => ({ activeProjectCwd: () => '/fallback' }));
vi.mock('@/lib/shell/shell-store', () => ({
	useShellStore: {
		getState: () => ({
			defaultEngineId: 'claude-code',
			activeProject: { id: 'royalti-co', root_path: '/work/royalti-co', extra_roots: [] },
		}),
	},
}));
vi.mock('@/lib/panes/pane-store', () => ({
	usePaneStore: { getState: () => ({ focusedId: 'x', root: { type: 'leaf', id: 'x', tabs: [], activeTabIdx: 0 } }) },
}));

import { resolveTarget, SEAT_GONE_REASON, targetEngineId } from './resolve-target';
import { useSeatNotice } from './seat-notice';

function seat(over: Partial<SeatView> = {}): SeatView {
	return {
		id: 'seat-1',
		project_id: 'royalti-co',
		name: 'docs',
		engine_id: 'claude-code',
		session: { kind: 'terminal', terminal_id: 'term-a', external_id: 'conv-abcdef123', cwd: '/work/docs' },
		created_at: 0,
		last_active_at: 0,
		hold: null,
		address: 'seat:royalti-co/docs',
		agent_id: 'seat-1',
		status: 'vacant',
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

const target = { kind: 'seat', seat_id: 'seat-1' } as const;
const notice = () => useSeatNotice.getState().notice?.message;

beforeEach(() => {
	ptys.clear();
	terminal.tabs = [];
	m.cachedSeats.mockReturnValue([seat()]);
	m.cachedSeat.mockReturnValue(seat());
	useSeatNotice.setState({ notice: null });
});

afterEach(() => {
	vi.clearAllMocks();
});

describe('resolveTarget — seat (label from the cache)', () => {
	it('resolves to a seat target carrying the cached seat and its engine', () => {
		const r = resolveTarget(target);
		expect(r.kind).toBe('seat');
		expect(r.engineId).toBe('claude-code');
		expect(r.seat?.name).toBe('docs');
		expect(m.seatsResolve).not.toHaveBeenCalled();
	});

	it('an unknown seat in a loaded roster is disabled: "That seat no longer exists"', async () => {
		m.cachedSeats.mockReturnValue([]);
		const r = resolveTarget(target);
		expect(r.kind).toBe('none');
		expect(r.disabledReason).toBe(SEAT_GONE_REASON);
		await expect(r.send('x')).rejects.toThrow(SEAT_GONE_REASON);
	});

	it('a roster not loaded yet keeps the target sendable, and fetches nothing during render', () => {
		m.cachedSeats.mockReturnValue(undefined);
		m.cachedSeat.mockReturnValue(undefined);
		const r = resolveTarget(target);
		expect(m.prefetchSeats).not.toHaveBeenCalled();
		expect(r.kind).toBe('seat');
	});

	it("'new run' keys start on the seat's engine", () => {
		expect(targetEngineId(target)).toBe('claude-code');
	});
});

describe('resolveTarget — seat send (route decided at send time)', () => {
	it('resolves with client ui and a resume claim request', async () => {
		m.seatsResolve.mockResolvedValue({ route: 'chi-resume', seat: seat({ status: 'idle' }), run_id: 'run-1', busy: false });
		await resolveTarget(target).send('go');
		expect(m.seatsResolve).toHaveBeenCalledWith({ seatId: 'seat-1' }, { client: 'ui' }, { claimResume: true });
	});

	it('pty → the existing PTY inject (no context line into an agent TUI)', async () => {
		ptys.set('term-a', { id: 'pty-7', mode: 'ephemeral', exited: false });
		terminal.tabs.push({ id: 'term-a', spec: { wrap: { engine: 'claude' } }, status: 'running' });
		m.seatsResolve.mockResolvedValue({
			route: 'pty',
			seat: seat({ status: 'live', agent: 'live' }),
			terminal_id: 'term-a',
			agent: 'live',
			lease_holder: null,
		} satisfies SeatRoute);
		await resolveTarget(target).send('fix the test', { project: '/work/royalti-co' });
		expect(m.ptyWrite).toHaveBeenCalledWith('pty-7', 'fix the test\r');
	});

	it('chi-resume, not busy → chiResume with the context appended', async () => {
		m.seatsResolve.mockResolvedValue({ route: 'chi-resume', seat: seat({ status: 'idle' }), run_id: 'run-1', busy: false });
		await resolveTarget(target).send('and the tests', { project: '/p' });
		expect(m.chiResume).toHaveBeenCalledWith('run-1', 'and the tests\n\nContext: project /p');
		expect(m.seatsQueue).not.toHaveBeenCalled();
	});

	it('chi-resume, busy → seatsQueue (never chi_resume) and the queue notice (§4.5)', async () => {
		m.seatsResolve.mockResolvedValue({ route: 'chi-resume', seat: seat({ name: 'nightly', status: 'run' }), run_id: 'run-1', busy: true });
		await resolveTarget(target).send('then deploy');
		expect(m.chiResume).not.toHaveBeenCalled();
		expect(m.seatsQueue).toHaveBeenCalledWith('seat-1', 'then deploy', { client: 'ui' });
		// E-4: subscribed before queueing, so a drop is never silent.
		expect(m.ensureSeatsLiveSync).toHaveBeenCalled();
		expect(notice()).toBe('Queued for @nightly — sends when its run finishes');
	});

	it('vacant with a claim → path T: a resumed agent terminal with the text as its first prompt, then seatsMove', async () => {
		m.seatsResolve.mockResolvedValue({
			route: 'vacant',
			seat: seat(),
			resume: { resumable: true },
			claim: 'claim-9',
		} satisfies SeatRoute);
		m.seatsMove.mockResolvedValue({ seat: seat({ status: 'live' }), from_seat_ids: [] });
		await resolveTarget(target).send('write the changelog');

		expect(m.buildAgentWrappedCmd).toHaveBeenCalledWith(
			expect.objectContaining({
				engine: 'claude',
				prompt: 'write the changelog',
				resumeSessionId: 'conv-abcdef123',
				terminalId: 'term-new-0001',
				cwd: '/work/docs',
			})
		);
		// In-process PTY: a daemon terminal can't be seated (P-10).
		expect(m.openTabPty).toHaveBeenCalledWith(
			expect.objectContaining({ id: 'term-new-0001', claudeSessionId: 'conv-abcdef123' }),
			{ forceEphemeral: true }
		);
		// The prompt is dropped from the tab's spec once spawned (no replay on respawn).
		const tab = terminal.tabs.find((t) => t.id === 'term-new-0001');
		expect((tab?.spec.wrap as { prompt: unknown }).prompt).toBeNull();
		expect(m.seatsMove).toHaveBeenCalledWith(
			{ kind: 'terminal', terminalId: 'term-new-0001', engineId: 'claude-code', cwd: '/work/docs', externalId: 'conv-abcdef123' },
			'seat-1',
			{ client: 'ui' },
			{ claim: 'claim-9' }
		);
		expect(m.seatsResume).not.toHaveBeenCalled();
		expect(notice()).toBe('docs was vacant — resumed session conv-abc, then sent');
	});

	it('vacant, not resumable, with a claim → path T starts a new agent and says so (§6.3)', async () => {
		m.seatsResolve.mockResolvedValue({
			route: 'vacant',
			seat: seat({ engine_id: 'codex', resume: { resumable: false, reason: 'no_resume_id' } }),
			resume: { resumable: false, reason: 'no_resume_id' },
			claim: 'claim-1',
		} satisfies SeatRoute);
		m.seatsMove.mockResolvedValue({ seat: seat(), from_seat_ids: [] });
		await resolveTarget(target).send('go');
		expect(m.buildAgentWrappedCmd).toHaveBeenCalledWith(
			expect.objectContaining({ engine: 'codex', resumeSessionId: null })
		);
		expect(m.seatsMove).toHaveBeenCalledWith(
			expect.objectContaining({ externalId: null }),
			'seat-1',
			{ client: 'ui' },
			{ claim: 'claim-1' }
		);
		expect(notice()).toBe("docs was vacant — its codex session can't be resumed; started a new one");
	});

	it('vacant without a claim → path H (E-1): seatsResume with fallback fresh', async () => {
		m.seatsResolve.mockResolvedValue({
			route: 'vacant',
			seat: seat({ engine_id: 'openrouter', session: { kind: 'run', run_id: 'run-3', external_id: null, cwd: null } }),
			resume: { resumable: false, reason: 'process_local' },
			claim: null,
		} satisfies SeatRoute);
		m.seatsResume.mockResolvedValue({
			seat: seat(),
			run_id: 'run-4abcdefgh',
			outcome: 'started-fresh',
			previous: null,
			reason: 'process_local',
		});
		await resolveTarget(target).send('summarise', { project: '/p' });
		expect(m.seatsResume).toHaveBeenCalledWith(
			'seat-1',
			'summarise\n\nContext: project /p',
			{ client: 'ui' },
			{ fallback: 'fresh' }
		);
		expect(m.openTabPty).not.toHaveBeenCalled();
		expect(notice()).toBe(
			"docs was vacant — its openrouter session can't resume after restart; started a new one"
		);
	});

	it('a seat_not_vacant race goes back to the resolve once', async () => {
		m.seatsResolve
			.mockResolvedValueOnce({ route: 'vacant', seat: seat(), resume: { resumable: true }, claim: null })
			.mockResolvedValueOnce({ route: 'chi-resume', seat: seat({ status: 'idle' }), run_id: 'run-1', busy: false });
		m.seatsResume.mockRejectedValueOnce({ code: 'seat_not_vacant', message: 'not vacant' });
		await resolveTarget(target).send('x');
		expect(m.seatsResolve).toHaveBeenCalledTimes(2);
		expect(m.chiResume).toHaveBeenCalledWith('run-1', 'x');
	});

	it('seat_held → the §5.2 refusal with Take over, which repeats the call with takeover: true', async () => {
		m.seatsResolve.mockRejectedValueOnce({
			code: 'seat_held',
			message: 'held',
			details: { client: 'iyke-orch', since: 0, expires_at: 1 },
		});
		await expect(resolveTarget(target).send('x')).rejects.toThrow(/is held by iyke-orch since .* — Take over to use it/);
		const shown = useSeatNotice.getState().notice;
		expect(shown?.message).toMatch(/^docs is held by iyke-orch since /);
		expect(shown?.action?.label).toBe('Take over');

		m.seatsResolve.mockResolvedValueOnce({ route: 'chi-resume', seat: seat({ status: 'idle' }), run_id: 'run-1', busy: false });
		await shown?.action?.run();
		expect(m.seatsResolve).toHaveBeenLastCalledWith(
			{ seatId: 'seat-1' },
			{ client: 'ui', takeover: true },
			{ claimResume: true }
		);
		expect(m.chiResume).toHaveBeenCalledWith('run-1', 'x');
	});

	it('path T spawn failure removes the half-made terminal tab', async () => {
		m.seatsResolve.mockResolvedValue({
			route: 'vacant',
			seat: seat(),
			resume: { resumable: true },
			claim: 'claim-9',
		} satisfies SeatRoute);
		m.openTabPty.mockRejectedValueOnce(new Error('spawn failed'));
		await expect(resolveTarget(target).send('x')).rejects.toThrow('spawn failed');
		expect(terminal.tabs.find((t) => t.id === 'term-new-0001')).toBeUndefined();
		expect(m.seatsMove).not.toHaveBeenCalled();
	});

	it('no path-T claim is asked for a cached seat on an engine with no terminal wrap', async () => {
		m.cachedSeat.mockReturnValue(seat({ engine_id: 'openrouter' }));
		m.seatsResolve.mockResolvedValue({ route: 'chi-resume', seat: seat({ status: 'idle' }), run_id: 'run-1', busy: false });
		await resolveTarget(target).send('go');
		expect(m.seatsResolve).toHaveBeenCalledWith({ seatId: 'seat-1' }, { client: 'ui' }, { claimResume: false });
	});

	it('seat_resuming → "@<name> is being resumed — try again shortly"', async () => {
		m.seatsResolve.mockRejectedValueOnce({ code: 'seat_resuming', message: 'docs is already being resumed' });
		await expect(resolveTarget(target).send('x')).rejects.toThrow('@docs is being resumed — try again shortly');
	});

	it('a seat_held refusal of the write after the resolve gets the §5.2 notice with Take over', async () => {
		m.seatsResolve.mockResolvedValue({ route: 'chi-resume', seat: seat({ name: 'nightly', status: 'run' }), run_id: 'run-1', busy: true });
		m.seatsQueue.mockRejectedValueOnce({
			code: 'seat_held',
			message: 'held',
			details: { client: 'iyke-orch', since: 0, expires_at: 1 },
		});
		await expect(resolveTarget(target).send('x')).rejects.toThrow(/is held by iyke-orch since .* — Take over to use it/);
		expect(useSeatNotice.getState().notice?.action?.label).toBe('Take over');
	});

	it('agent_not_live → "the agent in @<name>\'s terminal isn\'t running yet"', async () => {
		m.seatsResolve.mockRejectedValueOnce({ code: 'agent_not_live', message: 'starting' });
		await expect(resolveTarget(target).send('x')).rejects.toThrow(
			"the agent in @docs's terminal isn't running yet"
		);
	});

	it('send resolves to undefined — the notice says where the text went (ADR-021)', async () => {
		m.seatsResolve.mockResolvedValue({ route: 'chi-resume', seat: seat({ status: 'idle' }), run_id: 'run-1', busy: false });
		await expect(resolveTarget(target).send('go')).resolves.toBeUndefined();
	});
});
