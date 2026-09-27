// WP-66 — the notices a dispatch to a seat shows (G-SEATS §4.5, §5.2, §5.3,
// §6.3, §17 E-4). ADR-021: a notice says where the text went, never what
// came back.
//
// The store is module-level so `resolve-target.ts` can raise a notice from a
// `send` without a React context; `SeatNoticeHost` (`seat-notice-host.tsx`)
// renders it. The exact texts live here as pure functions.

import { create } from 'zustand';
import { onSeatsChanged } from '@/lib/queries/seats';
import type { NotResumableReason, SeatsChangedEvent } from '@/lib/tauri-cmd';

export interface SeatNoticeAction {
	label: string;
	run: () => void | Promise<void>;
}

export interface SeatNotice {
	/** Bumps on every notice so a repeat of the same text restarts the TTL. */
	seq: number;
	message: string;
	variant: 'info' | 'error';
	action?: SeatNoticeAction;
	/** How long it stays up; the host's default when absent. WP-67's Undo
	 *  toasts pass the 8 s undo window (G-SEATS §4.2). */
	ttlMs?: number;
}

export interface SeatNoticeOpts {
	variant?: 'info' | 'error';
	action?: SeatNoticeAction;
	ttlMs?: number;
}

interface SeatNoticeState {
	notice: SeatNotice | null;
	show: (message: string, opts?: SeatNoticeOpts) => void;
	dismiss: () => void;
}

let seq = 0;

export const useSeatNotice = create<SeatNoticeState>((set) => ({
	notice: null,
	show: (message, opts) =>
		set({
			notice: {
				seq: ++seq,
				message,
				variant: opts?.variant ?? 'info',
				...(opts?.action ? { action: opts.action } : {}),
				...(opts?.ttlMs ? { ttlMs: opts.ttlMs } : {}),
			},
		}),
	dismiss: () => set({ notice: null }),
}));

export function showSeatNotice(message: string, opts?: SeatNoticeOpts): void {
	useSeatNotice.getState().show(message, opts);
}

// ─── Texts (exact) ──────────────────────────────────────────────────────────

/** The outcome of a dispatch to a vacant seat (§6.3). */
export type VacantDispatchOutcome =
	| { outcome: 'resumed'; session: string }
	| { outcome: 'started-fresh'; reason: NotResumableReason | undefined; session: string };

/** §6.3, exact. `<N>` is the UI's label for the session (`session`). */
export function vacantDispatchText(name: string, engineId: string, result: VacantDispatchOutcome): string {
	if (result.outcome === 'resumed') return `${name} was vacant — resumed session ${result.session}, then sent`;
	switch (result.reason) {
		case 'process_local':
			return `${name} was vacant — its ${engineId} session can't resume after restart; started a new one`;
		case 'no_resume_support':
		case 'no_resume_id':
		case 'run_missing':
		case 'engine_unavailable':
			return `${name} was vacant — its ${engineId} session can't be resumed; started a new one`;
		case 'no_session':
		case undefined:
			return `${name} was vacant — filled with session ${result.session}, then sent`;
	}
}

/** §4.5, exact. */
export function queuedText(name: string): string {
	return `Queued for @${name} — sends when its run finishes`;
}

const QUEUE_DROPPED_REASON: Record<NonNullable<SeatsChangedEvent['queue_dropped']>, string> = {
	cleared: 'the seat was cleared',
	removed: 'the seat was removed',
	no_run: 'the seat has no run to send it to',
	run_missing: 'its run no longer exists',
	send_failed: 'sending it failed',
};

/** §17 E-4. */
export function queueDroppedText(name: string, reason: SeatsChangedEvent['queue_dropped']): string {
	const why = reason ? QUEUE_DROPPED_REASON[reason] : 'it was dropped';
	return `Your queued text for @${name} wasn't sent — ${why}`;
}

/** Local time of day for "since T" / "at T". */
export function formatSeatTime(ms: number): string {
	try {
		return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
	} catch {
		return new Date(ms).toISOString();
	}
}

/** §5.2, exact (UI form). */
export function seatHeldText(name: string, client: string, since: number): string {
	return `${name} is held by ${client} since ${formatSeatTime(since)} — Take over to use it`;
}

/** §5.3, exact. */
export function seatTakenOverText(name: string, by: string, at: number): string {
	return `${by} took over ${name} at ${formatSeatTime(at)}`;
}

/** §9.4 step 1, exact. */
export function agentNotLiveText(name: string): string {
	return `the agent in @${name}'s terminal isn't running yet`;
}

// E-4: a dropped queued text is never silent.
onSeatsChanged((event, seat) => {
	if (!event.kinds.includes('queue-dropped')) return;
	showSeatNotice(queueDroppedText(seat?.name ?? event.seat_id.slice(0, 8), event.queue_dropped), {
		variant: 'error',
	});
});
