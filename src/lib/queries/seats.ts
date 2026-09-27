// WP-66 — TanStack Query surface for Chi seats (G-SEATS §9.2 `seats_list`,
// §10 `seats://changed`).
//
// `useSeats(projectId)` is the one read of a project's roster. The dispatch
// target (`resolve-target.ts`) labels a `{kind:'seat'}` target and reports
// its `disabledReason` from this query's cache, synchronously — it never
// decides a route from it (§9.4: `send` asks `seats_resolve` at send time).
//
// Live updates: `seats://changed` fires once per affected seat after every
// committed write (§10). `ensureSeatsLiveSync()` subscribes once for the
// app's lifetime (idempotent) and invalidates the event's project list. The
// event is never a source of truth. The same listener turns an E-4
// `queue-dropped` event into the Companion's notice (G-SEATS §17 E-4).
//
// Liveness changes are not seat events (§10: "WP-66/67 invalidate the seats
// query on those signals"). An observed roster re-reads when a terminal's
// PTY status or agent liveness changes in the terminal store (which the
// store-level `hooks://event` listener keeps current), and polls while one
// of its seats has a Chi run in flight — there is no run-status event.

import { queryOptions, useQuery } from '@tanstack/react-query';
import { useEffect, useRef } from 'react';
import { queryClient } from '@/lib/query-client';
import { useTerminalStore } from '@/terminal/session-store';
import {
	listen,
	SEATS_CHANGED_EVENT,
	type SeatError,
	type SeatsChangedEvent,
	type SeatView,
	seatsList,
} from '@/lib/tauri-cmd';

/** §5.1: the shell UI is always client `ui`, and never holds unless it takes over. */
export const UI_SEAT_CLIENT = 'ui';

/** The typed rejection of a `seats*` call (§9.5), when `err` is one. Tauri
 *  rejects with the serialized `SeatError` object, not an `Error`. */
export function seatErrorOf(err: unknown): SeatError | null {
	if (err && typeof err === 'object' && typeof (err as { code?: unknown }).code === 'string') {
		const e = err as { code: string; message?: unknown; details?: unknown };
		return {
			code: e.code as SeatError['code'],
			message: typeof e.message === 'string' ? e.message : e.code,
			...(e.details && typeof e.details === 'object'
				? { details: e.details as SeatError['details'] }
				: {}),
		};
	}
	return null;
}

export const SEATS_QUERY_ROOT = ['seats'] as const;

/** `seats_list` for one project (explicit id — the cache is keyed by it). */
export function seatsQueryKey(projectId: string) {
	return ['seats', 'list', projectId] as const;
}

export function seatsQueryOptions(projectId: string) {
	return queryOptions({
		queryKey: seatsQueryKey(projectId),
		queryFn: () => seatsList(projectId),
		staleTime: 5_000,
	});
}

/** While an observed roster has a run in flight, re-read it this often: a
 *  run finishing changes the derived status with no seat write (§10). */
export const SEATS_RUN_POLL_MS = 5_000;

/** One string that changes whenever a terminal's PTY status or agent
 *  liveness does — the liveness signals §10 names for the terminal side. */
function terminalLivenessSignature(
	tabs: ReadonlyArray<{ id: string; status: string; agentLive?: boolean }>
): string {
	return tabs.map((t) => `${t.id}:${t.status}:${t.agentLive ? 1 : 0}`).join('|');
}

/** The roster of `projectId` (`null` → nothing is fetched). */
export function useSeats(projectId: string | null, opts: { enabled?: boolean } = {}) {
	const enabled = (opts.enabled ?? true) && Boolean(projectId);
	useEffect(() => {
		if (enabled) ensureSeatsLiveSync();
	}, [enabled]);
	// §10 liveness: re-read on a PTY exit / agent start or end. The first
	// render is not a change (the query's own fetch covers it).
	const liveness = useTerminalStore((s) => terminalLivenessSignature(s.tabs));
	const seen = useRef(liveness);
	useEffect(() => {
		if (seen.current === liveness) return;
		seen.current = liveness;
		if (enabled && projectId) void invalidateSeats(projectId);
	}, [liveness, enabled, projectId]);
	return useQuery({
		...seatsQueryOptions(projectId ?? ''),
		enabled,
		refetchInterval: (query) =>
			query.state.data?.some((s) => s.status === 'run') ? SEATS_RUN_POLL_MS : false,
	});
}

/** The cached roster of `projectId`; `undefined` when it was never loaded. */
export function cachedSeats(projectId: string): SeatView[] | undefined {
	return queryClient.getQueryData<SeatView[]>(seatsQueryKey(projectId));
}

/** A seat from any cached roster, by id. `undefined` when no loaded roster has it. */
export function cachedSeat(seatId: string): SeatView | undefined {
	for (const [, rows] of queryClient.getQueriesData<SeatView[]>({ queryKey: [...SEATS_QUERY_ROOT, 'list'] })) {
		const found = rows?.find((s) => s.id === seatId);
		if (found) return found;
	}
	return undefined;
}

/** Fetch `projectId`'s roster into the cache if it isn't there (fire-and-forget). */
export function prefetchSeats(projectId: string): void {
	ensureSeatsLiveSync();
	void queryClient.prefetchQuery(seatsQueryOptions(projectId)).catch(() => {});
}

export function invalidateSeats(projectId?: string): Promise<void> {
	return queryClient.invalidateQueries({
		queryKey: projectId ? seatsQueryKey(projectId) : SEATS_QUERY_ROOT,
	});
}

// ─── Live sync ──────────────────────────────────────────────────────────────

type SeatsEventHandler = (event: SeatsChangedEvent, seat: SeatView | undefined) => void;

const handlers = new Set<SeatsEventHandler>();
let started = false;

/** Observe every `seats://changed` event (the seat is read from the cache
 *  BEFORE it is invalidated, so a removed seat still has its name). */
export function onSeatsChanged(handler: SeatsEventHandler): () => void {
	handlers.add(handler);
	return () => handlers.delete(handler);
}

/** Subscribe to `seats://changed` once for the app's lifetime. */
export function ensureSeatsLiveSync(): void {
	if (started) return;
	started = true;
	try {
		void listen<SeatsChangedEvent>(SEATS_CHANGED_EVENT, (event) => {
			const payload = event.payload;
			const seat = cachedSeat(payload.seat_id);
			for (const handler of handlers) {
				try {
					handler(payload, seat);
				} catch (err) {
					console.error('[seats] change handler threw', err);
				}
			}
			void invalidateSeats(payload.project_id);
		}).catch(() => {
			// No event bridge (tests, a remote session without events): retry on
			// the next call instead of staying silently unsubscribed.
			started = false;
		});
	} catch {
		started = false;
	}
}

/** Test seam: forget the subscription (handlers stay registered). */
export function __resetSeatsLiveSyncForTests(): void {
	started = false;
}
