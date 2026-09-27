// WP-68 — pure helpers for the `/chi` seat board (D-09 `seats-board.html`,
// the SECONDARY seat surface; G-SEATS §1.6, §2.1, §5.2, §6.2, §7.3).
//
// The board draws the rail's model (`useSeatRoster`) and calls the rail's
// actions; nothing here re-derives a seat state or invents a figure. These
// functions only turn a `SeatView` / an unseated session into the words a
// board row, the detail column and the iyke line show, so the texts are
// testable without React.
//
// ADR-021: a board row carries STATE and ADDRESSES — never model output.

import type { SeatView } from '@/lib/tauri-cmd';
import type { Mount } from '@/shell/companion/seat-actions';
import { iykeSeatCreate, iykeSendToSeat, iykeVacant } from '@/shell/companion/seat-model';
import type { UnseatedSession } from '@/shell/companion/seat-roster';

// ─── Selection (board-local; browsing never retargets dispatch) ────────────

/** What the board has selected. Distinct from the rail's selection, which
 *  IS the dispatch target (G-SEATS §9.1); the board's only becomes the
 *  target through *Make dispatch target*. */
export type BoardSelection = { kind: 'seat'; id: string } | { kind: 'session'; id: string };

/** One row, in board order: every seat, then the Unseated group. */
export type BoardRow = { kind: 'seat'; seat: SeatView } | { kind: 'session'; session: UnseatedSession };

export function boardRows(seats: readonly SeatView[], unseated: readonly UnseatedSession[]): BoardRow[] {
	return [
		...seats.map((seat) => ({ kind: 'seat' as const, seat })),
		...unseated.map((session) => ({ kind: 'session' as const, session })),
	];
}

/** Stable key of a row (`data-row-key`, the roving focus target). */
export function rowKey(row: BoardRow | BoardSelection): string {
	if ('seat' in row) return `seat:${row.seat.id}`;
	if ('session' in row) return `session:${row.session.id}`;
	return `${row.kind}:${row.id}`;
}

export function selectionOf(row: BoardRow): BoardSelection {
	return row.kind === 'seat' ? { kind: 'seat', id: row.seat.id } : { kind: 'session', id: row.session.id };
}

/** The selection, when it still names a row; otherwise null (no detail column). */
export function validSelection(
	sel: BoardSelection | null,
	seats: readonly SeatView[],
	unseated: readonly UnseatedSession[]
): BoardSelection | null {
	if (!sel) return null;
	if (sel.kind === 'seat') return seats.some((s) => s.id === sel.id) ? sel : null;
	return unseated.some((u) => u.id === sel.id) ? sel : null;
}

/** ↑ ↓ Home End over the rows (clamped, like the rail — no wrap). */
export function moveSelection(
	rows: readonly BoardRow[],
	current: BoardSelection | null,
	key: 'ArrowDown' | 'ArrowUp' | 'Home' | 'End'
): BoardSelection | null {
	if (rows.length === 0) return null;
	const at = current ? rows.findIndex((r) => rowKey(r) === rowKey(current)) : -1;
	let next: number;
	if (key === 'Home') next = 0;
	else if (key === 'End') next = rows.length - 1;
	else if (key === 'ArrowDown') next = at < 0 ? 0 : Math.min(rows.length - 1, at + 1);
	else next = at < 0 ? 0 : Math.max(0, at - 1);
	const row = rows[next];
	return row ? selectionOf(row) : null;
}

// ─── Readouts ───────────────────────────────────────────────────────────────

/** The head's count line: `4 seats · 1 vacant · 1 unseated session`. */
export function countLine(seats: readonly SeatView[], unseatedCount: number): string {
	const n = seats.length;
	const vacant = seats.filter((s) => s.status === 'vacant').length;
	const parts = [n ? `${n} seat${n === 1 ? '' : 's'}` : 'no seats'];
	if (vacant) parts.push(`${vacant} vacant`);
	parts.push(`${unseatedCount} unseated session${unseatedCount === 1 ? '' : 's'}`);
	return parts.join(' · ');
}

/** The state cell's word (G-SEATS §2.1). No invented duration: an idle or
 *  run time the host doesn't report is not shown (D-09 revision 2). */
export function stateWord(seat: Pick<SeatView, 'status' | 'agent'>): string {
	if (seat.status === 'live' && seat.agent === 'starting') return 'live · starting';
	return seat.status;
}

/** The mount readout a board cell shows — `short` in a normal pane, `long`
 *  when the pane is wide (a container query). Never part of the address. */
export interface MountText {
	short: string;
	long: string;
	where: 'main' | 'window' | 'headless' | 'none' | 'vacant';
}

export function mountText(mount: Mount, opts: { isRun: boolean; vacant: boolean }): MountText {
	if (opts.vacant) return { short: '—', long: '—', where: 'vacant' };
	if (mount.where === 'window') {
		return { short: 'Window 2', long: 'Window 2 — popped out', where: 'window' };
	}
	if (mount.where === 'main') {
		return { short: `pane ${mount.paneIndex}`, long: `main window · pane ${mount.paneIndex}`, where: 'main' };
	}
	if (opts.isRun) return { short: 'headless', long: 'not mounted (headless)', where: 'headless' };
	return { short: 'not mounted', long: 'not mounted', where: 'none' };
}

/** A stable string of a mount, to notice it change (the "moved" highlight). */
export function mountSignature(m: Mount): string {
	if (m.where === 'window') return `window:${m.label}`;
	if (m.where === 'main') return `main:${m.leafId}`;
	return 'none';
}

/** G-93 / G-96: a row "moved" when its mount became a window it wasn't in. */
export function movedToWindow(prev: string | undefined, next: string): boolean {
	return prev !== undefined && prev !== next && next.startsWith('window:');
}

/** The scratchpad cell: its latest entry and count, or "empty". */
export function padLatest(seat: Pick<SeatView, 'pad'>): { latest: string | null; count: string } {
	const n = seat.pad.count;
	return {
		latest: seat.pad.latest ? `“${seat.pad.latest.name}”` : null,
		count: n ? `${n} ${n === 1 ? 'entry' : 'entries'}` : 'empty',
	};
}

/** How long ago `ms` was, in the rail's words (`2h ago`). */
export function relativeTime(ms: number, now = Date.now()): string {
	const s = Math.max(0, Math.round((now - ms) / 1000));
	if (s < 60) return 'just now';
	const m = Math.round(s / 60);
	if (m < 60) return `${m}m ago`;
	const h = Math.round(m / 60);
	if (h < 24) return `${h}h ago`;
	return `${Math.round(h / 24)}d ago`;
}

// ─── The iyke form (Principle 5, G-SEATS §7.3) ─────────────────────────────

/** The board's iyke line: what a caller would type for the selected row. */
export function boardIyke(
	sel: BoardRow | null,
	projectId: string
): string {
	if (!sel) return `seat ls --project ${projectId}`;
	if (sel.kind === 'session') return iykeSeatCreate('', sel.session.engineId ?? '<engine>', { kind: 'open', ref: sel.session.id });
	const seat = sel.seat;
	if (seat.status !== 'vacant') return iykeSendToSeat(seat.name, '');
	return iykeVacant(seat.name, Boolean(seat.session) && seat.resume.resumable);
}

// ─── State map (G-55 `data-state` on the board root) ───────────────────────

export type BoardState =
	| 'board-loading'
	| 'board-error'
	| 'board-empty'
	| 'board-create'
	| 'board-popout'
	| 'board-vacant'
	| 'board-roster';

/**
 * The root's `data-state`. D-09's seven states map as: `roster` →
 * `board-roster`; `empty` → `board-empty`; `create` → `board-create` (the
 * Companion form is open beside the board); `vacant` → `board-vacant` (a
 * vacant seat selected); `popout` → `board-popout` (a row's "moved"
 * highlight is running); `dispatch` → the rail's picker (`seats-dispatch`)
 * over a `board-roster` board; `rest` → no board (the Companion strip's
 * `seats-rest`).
 */
export function boardState(opts: {
	state: 'loading' | 'error' | 'ready';
	seatCount: number;
	formOpen: boolean;
	moving: boolean;
	selectedVacant: boolean;
}): BoardState {
	if (opts.state === 'loading') return 'board-loading';
	if (opts.state === 'error') return 'board-error';
	if (opts.formOpen) return 'board-create';
	if (opts.seatCount === 0) return 'board-empty';
	if (opts.moving) return 'board-popout';
	if (opts.selectedVacant) return 'board-vacant';
	return 'board-roster';
}
