// WP-66 — the seat-dispatch notices (G-SEATS §4.5, §5.2, §5.3, §6.3, E-4),
// the `seats://changed` live sync, and the project-switch target reset (P-3).

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const m = vi.hoisted(() => ({
	handler: null as null | ((e: { event: string; payload: unknown }) => void),
	listen: vi.fn(),
	seatsList: vi.fn(async () => []),
}));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	listen: m.listen,
	seatsList: m.seatsList,
}));

import { queryClient } from '@/lib/query-client';
import {
	__resetSeatsLiveSyncForTests,
	cachedSeat,
	ensureSeatsLiveSync,
	seatErrorOf,
	seatsQueryKey,
} from '@/lib/queries/seats';
import { useShellStore } from '@/lib/shell/shell-store';
import type { SeatView } from '@/lib/tauri-cmd';
import { targetAfterProjectSwitch } from './companion-store';
import {
	queueDroppedText,
	queuedText,
	seatHeldText,
	seatTakenOverText,
	useSeatNotice,
	vacantDispatchText,
} from './seat-notice';

function seat(over: Partial<SeatView> = {}): SeatView {
	return {
		id: 'seat-1',
		project_id: 'royalti-co',
		name: 'nightly',
		engine_id: 'claude-code',
		session: null,
		created_at: 0,
		last_active_at: 0,
		hold: null,
		address: 'seat:royalti-co/nightly',
		agent_id: 'seat-1',
		status: 'run',
		agent: null,
		resume: { resumable: true },
		engine_resume: 'durable',
		mount: null,
		queued: { since: 0 },
		pad: { count: 0, latest: null },
		inbox_count: 0,
		...over,
	};
}

beforeEach(() => {
	queryClient.clear();
	useSeatNotice.setState({ notice: null });
	m.listen.mockImplementation(async (_event: string, handler: typeof m.handler) => {
		m.handler = handler;
		return () => {};
	});
});

afterEach(() => {
	vi.clearAllMocks();
	// Keep the module-level queue-dropped handler registered; only reset the
	// subscription so the next test re-subscribes.
	m.handler = null;
});

describe('§6.3 vacant-dispatch texts (exact)', () => {
	it('resumed', () => {
		expect(vacantDispatchText('docs', 'claude-code', { outcome: 'resumed', session: '2' })).toBe(
			'docs was vacant — resumed session 2, then sent'
		);
	});
	it('started fresh — process-local engine', () => {
		expect(
			vacantDispatchText('docs', 'openrouter', { outcome: 'started-fresh', reason: 'process_local', session: '3' })
		).toBe("docs was vacant — its openrouter session can't resume after restart; started a new one");
	});
	it.each(['no_resume_support', 'no_resume_id', 'run_missing', 'engine_unavailable'] as const)(
		'started fresh — %s',
		(reason) => {
			expect(vacantDispatchText('docs', 'pi', { outcome: 'started-fresh', reason, session: '3' })).toBe(
				"docs was vacant — its pi session can't be resumed; started a new one"
			);
		}
	);
	it('started fresh — never had a session: the rail\'s "filled with" template', () => {
		expect(vacantDispatchText('docs', 'codex', { outcome: 'started-fresh', reason: 'no_session', session: '1' })).toBe(
			'docs was vacant — filled with session 1, then sent'
		);
	});
});

describe('queue, hold and takeover texts', () => {
	it('§4.5 queue', () => {
		expect(queuedText('nightly')).toBe('Queued for @nightly — sends when its run finishes');
	});
	it('E-4 queue dropped', () => {
		expect(queueDroppedText('nightly', 'cleared')).toBe(
			"Your queued text for @nightly wasn't sent — the seat was cleared"
		);
	});
	it('§5.2 held / §5.3 taken over name the client and the time', () => {
		expect(seatHeldText('lead', 'iyke-orch', 0)).toMatch(
			/^lead is held by iyke-orch since .+ — Take over to use it$/
		);
		expect(seatTakenOverText('lead', 'iyke-orch', 0)).toMatch(/^iyke-orch took over lead at .+$/);
	});
});

describe('seats://changed live sync', () => {
	it('subscribes once, and invalidates the event project roster', () => {
		queryClient.setQueryData(seatsQueryKey('royalti-co'), [seat()]);
		const spy = vi.spyOn(queryClient, 'invalidateQueries');
		__resetSeatsLiveSyncForTests();
		ensureSeatsLiveSync();
		ensureSeatsLiveSync();
		expect(m.listen).toHaveBeenCalledTimes(1);
		expect(m.listen).toHaveBeenCalledWith('seats://changed', expect.any(Function));
		m.handler?.({
			event: 'seats://changed',
			payload: { project_id: 'royalti-co', seat_id: 'seat-1', kinds: ['updated'] },
		});
		expect(spy).toHaveBeenCalledWith({ queryKey: seatsQueryKey('royalti-co') });
		expect(useSeatNotice.getState().notice).toBeNull();
		spy.mockRestore();
	});

	it('E-4: a queue-dropped event shows the notice, named from the cache', () => {
		queryClient.setQueryData(seatsQueryKey('royalti-co'), [seat()]);
		expect(cachedSeat('seat-1')?.name).toBe('nightly');
		__resetSeatsLiveSyncForTests();
		ensureSeatsLiveSync();
		m.handler?.({
			event: 'seats://changed',
			payload: {
				project_id: 'royalti-co',
				seat_id: 'seat-1',
				kinds: ['cleared', 'queue-dropped'],
				queue_dropped: 'cleared',
			},
		});
		expect(useSeatNotice.getState().notice?.message).toBe(
			"Your queued text for @nightly wasn't sent — the seat was cleared"
		);
		expect(useSeatNotice.getState().notice?.variant).toBe('error');
	});
});

describe('seatErrorOf', () => {
	it('reads the serialized SeatError a Tauri command rejects with', () => {
		expect(seatErrorOf({ code: 'seat_held', message: 'held', details: { client: 'x', since: 1, expires_at: 2 } })).toEqual({
			code: 'seat_held',
			message: 'held',
			details: { client: 'x', since: 1, expires_at: 2 },
		});
		expect(seatErrorOf(new Error('boom'))).toBeNull();
		expect(seatErrorOf('boom')).toBeNull();
	});
});

describe('project switch (P-3)', () => {
	it('a seat target not in the new roster resets to the default target', () => {
		expect(targetAfterProjectSwitch({ kind: 'seat', seat_id: 'seat-1' }, undefined)).toEqual({
			kind: 'new',
			engine_id: null,
		});
		expect(targetAfterProjectSwitch({ kind: 'seat', seat_id: 'seat-1' }, [{ id: 'seat-2' }])).toEqual({
			kind: 'new',
			engine_id: null,
		});
	});

	it('a seat in the new roster, and every other target, is kept', () => {
		const t = { kind: 'seat', seat_id: 'seat-1' } as const;
		expect(targetAfterProjectSwitch(t, [{ id: 'seat-1' }])).toBe(t);
		const s = { kind: 'session', session_id: 'x' } as const;
		expect(targetAfterProjectSwitch(s, undefined)).toBe(s);
	});

	it('switching the active project resets a seat target (store subscription)', () => {
		useShellStore.setState({ activeProject: { id: 'royalti-co', root_path: null, extra_roots: [] } });
		useShellStore.setState({ companion: { activeTarget: { kind: 'seat', seat_id: 'seat-1' } } });
		// Same project: kept.
		useShellStore.setState({ activeProject: { id: 'royalti-co', root_path: '/x', extra_roots: [] } });
		expect(useShellStore.getState().companion.activeTarget).toEqual({ kind: 'seat', seat_id: 'seat-1' });
		// Another project whose roster doesn't have the seat: reset.
		useShellStore.setState({ activeProject: { id: 'other', root_path: null, extra_roots: [] } });
		expect(useShellStore.getState().companion.activeTarget).toEqual({ kind: 'new', engine_id: null });
	});
});
