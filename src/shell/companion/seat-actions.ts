// WP-67 — what the seat rail's controls do (D-09 `seats-companion.html`,
// G-SEATS §2.4, §4.1–§4.4, §5, §9.1, §9.3). Every write goes through a
// `seats*` wrapper in `tauri-cmd.ts` or an existing shell path (terminal
// kill, `chiCancel`, the pane store); nothing here is a new host call.
//
// ADR-021: the notices say where something went or what changed on the
// seat, never what an agent said.

import { create } from 'zustand';
import { usePaneStore } from '@/lib/panes/pane-store';
import { findLeaf, getLeafIdsInOrder, type MoveTabMode } from '@/lib/panes/pane-reducer';
import type { PaneNode } from '@/lib/panes/types';
import { cachedSeat, cachedSeats, invalidateSeats, seatErrorOf, UI_SEAT_CLIENT } from '@/lib/queries/seats';
import { type CompanionTarget, useShellStore } from '@/lib/shell/shell-store';
import {
	chiCancel,
	ptyKill,
	type SeatActor,
	type SeatSessionRef,
	type SeatView,
	seatsClear,
	seatsCreate,
	seatsGet,
	seatsRelease,
	seatsRemove,
	seatsRename,
	seatsResolve,
} from '@/lib/tauri-cmd';
import { reclaimSurface, useDetachedSurfaces } from '@/lib/window/detached-surfaces';
import { requestBoardFocus } from '@/shell/chi-board/board-ui';
import { getPty } from '@/terminal/pty-registry';
import { useTerminalStore } from '@/terminal/session-store';
import { type RailSelection, useCompanionStore } from './companion-store';
import { occupyVacantSeat } from './resolve-target';
import { formatSeatTime, showSeatNotice } from './seat-notice';
import { atName, SEAT_UNDO_MS, seatSessionRef } from './seat-model';
import {
	__resetPendingClearsForTests,
	armPendingClear,
	cancelPendingClear,
	flushPendingClear,
	hasPendingClear,
} from './seat-pending';
import { seatSessionNumberText, sessionName } from './seat-sessions';

const ACTOR: SeatActor = { client: UI_SEAT_CLIENT };

function errorText(err: unknown): string {
	const seat = seatErrorOf(err);
	if (seat) return seat.message;
	return err instanceof Error ? err.message : String(err);
}

function fail(err: unknown): void {
	showSeatNotice(errorText(err), { variant: 'error' });
}

// ─── UI state the rail, the strip and the form share ───────────────────────

/** How the create form opens. `seatSession` = *Seat this session…* (engine locked). */
export type SeatFormInit = { seatSession?: string | null };

interface SeatUiState {
	/** The New-seat form, inline in the Companion (D-09 `create`). */
	form: SeatFormInit | null;
	/** Seat id in inline rename (F2 / *Rename…*). */
	renaming: string | null;
	/** Seat ids inside their 8 s Remove undo window — hidden from the rail. */
	removing: Record<string, true>;
	/** Seat ids inside their 8 s Clear undo window — shown with no history. */
	clearing: Record<string, true>;
	/** Seat id awaiting the Remove confirm dialog. */
	confirmRemove: string | null;
}

export const useSeatUi = create<SeatUiState>(() => ({
	form: null,
	renaming: null,
	removing: {},
	clearing: {},
	confirmRemove: null,
}));

export function openSeatForm(init: SeatFormInit = {}): void {
	useSeatUi.setState({ form: init, renaming: null });
	useCompanionStore.getState().setState('expanded');
}

/** Close the New-seat form. Focus goes back to the *New seat* button (D-09
 *  `closeForm`) unless the caller moves it elsewhere (a created seat). */
export function closeSeatForm(opts: { returnFocus?: boolean } = {}): void {
	useSeatUi.setState({ form: null });
	if (opts.returnFocus === false) return;
	// The rail remounts on the next commit; focus its button then.
	setTimeout(() => {
		document.querySelector<HTMLElement>('[data-new-seat]')?.focus();
	}, 0);
}

export function startRename(seatId: string): void {
	useSeatUi.setState({ renaming: seatId });
}

export function cancelRename(): void {
	useSeatUi.setState({ renaming: null });
}

// ─── Selection = target + panel scope (§9.1) ────────────────────────────────

export function selectSeat(seat: SeatView): void {
	useCompanionStore.getState().selectRail({ kind: 'seat', seat_id: seat.id }, seatSessionRef(seat));
}

export function selectSession(sessionId: string): void {
	useCompanionStore.getState().selectRail({ kind: 'session', session_id: sessionId }, sessionId);
}

/** *Make dispatch target* — the same act as selecting (D-09: selection ≡ target). */
export function makeTarget(sel: RailSelection, scope: string | null): void {
	useCompanionStore.getState().selectRail(sel, scope);
}

// ─── Where a session is mounted (a readout, never part of the address) ─────

export type Mount =
	| { where: 'main'; paneIndex: number; leafId: string }
	| { where: 'window'; label: string }
	| { where: 'none' };

function leafHolding(root: PaneNode, terminalId: string): { leafId: string; tabIdx: number } | null {
	for (const leafId of getLeafIdsInOrder(root)) {
		const leaf = findLeaf(root, leafId);
		const tabIdx = leaf?.tabs.findIndex((t) => t.kind === 'terminal' && t.sessionId === terminalId) ?? -1;
		if (tabIdx >= 0) return { leafId, tabIdx };
	}
	return null;
}

/** Where a terminal is mounted: popped out, in a main-window pane, or nowhere. */
export function mountOfTerminal(
	terminalId: string,
	root: PaneNode,
	surfaceToWindow: Record<string, string>,
	ptyId: string | null | undefined
): Mount {
	if (ptyId) {
		const label = surfaceToWindow[`terminal:${ptyId}`];
		if (label) return { where: 'window', label };
	}
	const hit = leafHolding(root, terminalId);
	if (hit) {
		return { where: 'main', paneIndex: getLeafIdsInOrder(root).indexOf(hit.leafId) + 1, leafId: hit.leafId };
	}
	return { where: 'none' };
}

// ─── Panes ──────────────────────────────────────────────────────────────────

/** *Open in pane*: bring a popped-out terminal back, focus the pane that
 *  already holds it, or open it in the focused pane. */
export function openSessionInPane(terminalId: string): void {
	const tab = useTerminalStore.getState().tabs.find((t) => t.id === terminalId);
	if (tab?.ptyId) {
		const surfaceId = `terminal:${tab.ptyId}`;
		if (useDetachedSurfaces.getState().surfaceToWindow[surfaceId]) {
			void reclaimSurface(surfaceId);
		}
	}
	const panes = usePaneStore.getState();
	panes.addTab(panes.focusedId, { kind: 'terminal', sessionId: terminalId });
}

/**
 * The drag-a-seat-onto-a-pane gesture (D-09 Explorer variant, ported): a
 * centre drop mounts the terminal as a tab there, an edge drop splits. A
 * terminal already in a pane is MOVED there (one mount), not duplicated.
 */
export function mountTerminalAt(
	terminalId: string,
	paneId: string,
	mode: MoveTabMode,
	name: string | null
): boolean {
	const panes = usePaneStore.getState();
	const hit = leafHolding(panes.root, terminalId);
	let ok: boolean;
	if (hit && hit.leafId === paneId && mode === 'append') {
		panes.switchTab(paneId, hit.tabIdx);
		panes.focusPane(paneId);
		ok = true;
	} else if (hit) {
		panes.moveTab(hit.leafId, hit.tabIdx, paneId, mode);
		ok = true;
	} else {
		ok = panes.placeView(paneId, { kind: 'terminal', sessionId: terminalId }, mode);
	}
	if (ok && name) {
		const after = usePaneStore.getState();
		const at = leafHolding(after.root, terminalId);
		const n = at ? getLeafIdsInOrder(after.root).indexOf(at.leafId) + 1 : null;
		showSeatNotice(`${name} is now in ${n ? `pane ${n}` : 'a pane'} — its address is unchanged`);
	}
	return ok;
}

/** *Open scratchpad* — a scratchpad pane view on the seat's own scope (§9.3, §6a). */
export function openSeatScratchpad(seat: SeatView): void {
	const panes = usePaneStore.getState();
	panes.addTab(panes.focusedId, {
		kind: 'scratchpad',
		scope: seat.address,
		name: seat.pad.latest?.name ?? seat.name,
	});
}

/** *All seats* ⊞ — the `/chi` seat board (WP-68) in the focused pane; one tab.
 *  Every entry point lands keyboard focus on the board's selected row (D-09
 *  ENTRY), so this asks the board for it (`board-ui.ts`, a leaf module). */
export function openSeatBoard(): void {
	const panes = usePaneStore.getState();
	panes.addTab(panes.focusedId, { kind: 'route', path: '/chi' });
	requestBoardFocus();
}

export function copyText(text: string, message: string): void {
	try {
		void navigator.clipboard?.writeText(text).catch(() => {});
	} catch {
		// no clipboard in this context
	}
	showSeatNotice(message);
}

// ─── End session (T5: the seat stays, vacant) ───────────────────────────────

async function killTerminal(terminalId: string): Promise<void> {
	const pty = getPty(terminalId);
	if (pty) {
		await pty.kill();
		return;
	}
	const tab = useTerminalStore.getState().tabs.find((t) => t.id === terminalId);
	if (tab?.ptyId) await ptyKill(tab.ptyId);
}

export async function endSeatSession(seat: SeatView): Promise<void> {
	const s = seat.session;
	if (!s) return;
	const label = s.kind === 'terminal' ? sessionName(s.terminal_id) : sessionName(s.run_id);
	try {
		if (s.kind === 'terminal') await killTerminal(s.terminal_id);
		else await chiCancel(s.run_id);
		showSeatNotice(`Ended ${label} — ${atName(seat.name)} is vacant; its address and scratchpad stay`);
	} catch (err) {
		fail(err);
	} finally {
		void invalidateSeats(seat.project_id);
	}
}

export async function endUnseatedSession(terminalId: string): Promise<void> {
	try {
		await killTerminal(terminalId);
		showSeatNotice(`Ended ${sessionName(terminalId)} — it had no seat, so nothing keeps its name`);
	} catch (err) {
		fail(err);
	}
}

// ─── Clear / Remove, with the 8 s client-side Undo (§4.2) ──────────────────

/** Pending Removes. Pending Clears live in `seat-pending.ts`, which the
 *  dispatch path settles before it routes a send. */
const undoTimers = new Map<string, ReturnType<typeof setTimeout>>();

function pendingKey(kind: 'remove', seatId: string): string {
	return `${kind}:${seatId}`;
}

function dropPending(kind: 'clear' | 'remove', seatId: string): void {
	useSeatUi.setState((s) => {
		const field = kind === 'clear' ? 'clearing' : 'removing';
		const next = { ...s[field] };
		delete next[seatId];
		return { [field]: next } as Partial<SeatUiState>;
	});
}

/**
 * *Clear seat*: forget the session history, keep the scratchpad (DEC-69b).
 * The host call waits out the 8 s Undo window. A dispatch, *Fill* or
 * *Remove* inside the window commits it first (`flushPendingClear`); and
 * when it commits, a seat whose session changed since Clear was pressed
 * (another client started one) is left alone — Clear never ends up
 * unseating a session that is newer than the history it was meant to drop.
 */
export function clearSeat(seat: SeatView): void {
	if (hasPendingClear(seat.id)) return;
	const clearedRef = seatSessionRef(seat);
	useSeatUi.setState((s) => ({ clearing: { ...s.clearing, [seat.id]: true } }));
	armPendingClear(seat.id, SEAT_UNDO_MS, async () => {
		try {
			const current = await seatsGet({ seatId: seat.id }).catch(() => null);
			if (current && seatSessionRef(current) !== clearedRef) {
				showSeatNotice(`${atName(seat.name)} has a new session since you cleared it — left as it is`);
				return;
			}
			await seatsClear(seat.id, ACTOR);
		} catch (err) {
			fail(err);
		} finally {
			await invalidateSeats(seat.project_id).catch(() => {});
			dropPending('clear', seat.id);
		}
	});
	showSeatNotice(
		`Cleared ${seat.name} — session history forgotten; scratchpad ${seat.address} kept`,
		{
			ttlMs: SEAT_UNDO_MS,
			action: {
				label: 'Undo',
				run: () => {
					cancelPendingClear(seat.id);
					dropPending('clear', seat.id);
				},
			},
		}
	);
}

/**
 * *Remove seat…* after the confirm (§4.2, §14 N-1 default): the seat goes,
 * its pad goes (`removeMemory: true`, as the locked dialog says), a running
 * session keeps running, unseated. `next` is where the selection moves when
 * the removed seat held it.
 */
export function removeSeat(seat: SeatView, next: { sel: RailSelection; scope: string | null } | null): void {
	const key = pendingKey('remove', seat.id);
	if (undoTimers.has(key)) return;
	// A Clear still in its window commits now: the user asked for both, and
	// its timer must not fire against a seat that is gone (`seat_not_found`).
	void flushPendingClear(seat.id);
	const companion = useCompanionStore.getState();
	const target = useShellStore.getState().companion.activeTarget;
	const wasSelected = companion.railSelection?.kind === 'seat' && companion.railSelection.seat_id === seat.id;
	const wasTarget = target.kind === 'seat' && target.seat_id === seat.id;
	useSeatUi.setState((s) => ({ removing: { ...s.removing, [seat.id]: true }, confirmRemove: null }));
	if (wasSelected || wasTarget) {
		if (next) companion.selectRail(next.sel, next.scope);
		else {
			useCompanionStore.setState({ railSelection: null, panelScopeSessionId: null });
			useShellStore.getState().setCompanionTarget({ kind: 'new', engine_id: null });
		}
	}
	undoTimers.set(
		key,
		setTimeout(() => {
			undoTimers.delete(key);
			seatsRemove(seat.id, { removeMemory: true }, ACTOR)
				.catch(fail)
				.finally(() => {
					void invalidateSeats(seat.project_id).finally(() => dropPending('remove', seat.id));
				});
		}, SEAT_UNDO_MS)
	);
	showSeatNotice(`Removed seat ${seat.name}`, {
		ttlMs: SEAT_UNDO_MS,
		action: {
			label: 'Undo',
			run: () => {
				const t = undoTimers.get(key);
				if (t) clearTimeout(t);
				undoTimers.delete(key);
				dropPending('remove', seat.id);
				selectSeat(seat);
			},
		},
	});
}

/** Test seam: cancel every pending Clear / Remove without calling the host. */
export function __resetSeatUndoForTests(): void {
	for (const t of undoTimers.values()) clearTimeout(t);
	undoTimers.clear();
	__resetPendingClearsForTests();
	useSeatUi.setState({ form: null, renaming: null, removing: {}, clearing: {}, confirmRemove: null });
}

// ─── Rename (T8) ────────────────────────────────────────────────────────────

/** Returns an error message for the inline field, or null on success. */
export async function renameSeat(seat: SeatView, name: string): Promise<string | null> {
	try {
		await seatsRename(seat.id, name, ACTOR);
		useSeatUi.setState({ renaming: null });
		showSeatNotice(
			`Renamed @${seat.name} → @${name}. Callers still using --seat ${seat.name} now get “no such seat”`
		);
		return null;
	} catch (err) {
		return errorText(err);
	} finally {
		void invalidateSeats(seat.project_id);
	}
}

// ─── Holds (§5.2, §5.5) ─────────────────────────────────────────────────────

/** *Take over* — explicit, shown only while another client holds the seat. */
export async function takeOverSeat(seat: SeatView): Promise<void> {
	const from = seat.hold?.client ?? 'another client';
	try {
		await seatsResolve({ seatId: seat.id }, { ...ACTOR, takeover: true });
		showSeatNotice(`Took over ${seat.name} from ${from} at ${formatSeatTime(Date.now())}`);
	} catch (err) {
		fail(err);
	} finally {
		void invalidateSeats(seat.project_id);
	}
}

/** Drop the UI's own hold (after a takeover). */
export async function releaseSeat(seat: SeatView): Promise<void> {
	try {
		await seatsRelease(seat.id, ACTOR);
		showSeatNotice(`Released ${seat.name}`);
	} catch (err) {
		fail(err);
	} finally {
		void invalidateSeats(seat.project_id);
	}
}

// ─── Resume / Fill (§9.3, path T without a first turn) ─────────────────────

export async function resumeSeat(seat: SeatView): Promise<void> {
	const label = `session ${seatSessionNumberText(seat.session)}`;
	// Resuming the history a pending Clear would drop is an implicit Undo.
	if (cancelPendingClear(seat.id)) dropPending('clear', seat.id);
	try {
		const r = await occupyVacantSeat(seat.id, 'resume');
		selectSeat(r.seat);
		showSeatNotice(`Resumed ${label} in ${atName(seat.name)}`);
	} catch (err) {
		fail(err);
	}
}

export async function fillSeat(seat: SeatView): Promise<void> {
	try {
		// Filling inside a Clear window: the history goes first, as the rail shows.
		await flushPendingClear(seat.id);
		const r = await occupyVacantSeat(seat.id, 'fill');
		selectSeat(r.seat);
		showSeatNotice(`Filled ${atName(seat.name)} with ${sessionName(r.terminalId)}`);
	} catch (err) {
		fail(err);
	}
}

// ─── Create (T1 / T2a / T2b) ────────────────────────────────────────────────

/** §4.3: a move unbinds the session from whatever seat held it. Say which,
 *  so a seat left vacant — possibly in another project — is never silent. */
export function leftSeatsText(fromSeatIds: readonly string[] | undefined, projectId: string): string {
	if (!fromSeatIds?.length) return '';
	const names = fromSeatIds.map((id) => {
		const s = cachedSeat(id);
		if (!s) return 'another seat';
		return s.project_id === projectId ? atName(s.name) : s.address;
	});
	return ` — it left ${names.join(', ')}, which is now vacant`;
}

export type CreateSeatStart =
	| { kind: 'new' }
	/** A past session of another (vacant) seat — the move takes it (DEC-69c). */
	| { kind: 'resume'; from: SeatView }
	/** An open, unseated terminal (*Seat this session…*). */
	| { kind: 'open'; session: SeatSessionRef };

/**
 * Create a seat, then occupy it the way *Start with* says. Resolves to the
 * new seat's view (for selection); rejects with a message for the form.
 */
export async function createSeat(req: {
	projectId: string;
	name: string;
	engineId: string;
	/** The engine can run in a terminal (§6.1 wrap id) — path T is possible. */
	hasWrap: boolean;
	start: CreateSeatStart;
}): Promise<SeatView> {
	const { projectId, name, engineId, start } = req;
	try {
		if (start.kind === 'open') {
			const r = await seatsCreate(
				{ projectId, name, engineId, start: { kind: 'session', session: start.session } },
				ACTOR
			);
			const ref = start.session.kind === 'terminal' ? start.session.terminalId : start.session.runId;
			showSeatNotice(
				`Seat @${name} created around ${sessionName(ref)}${leftSeatsText(r.from_seat_ids, projectId)} · scratchpad ${r.seat.address}`
			);
			return r.seat;
		}
		const past = start.kind === 'resume' ? start.from.session : null;
		// A past RUN moves in as it is and resumes on the next send (§7.2).
		if (past?.kind === 'run') {
			const r = await seatsCreate(
				{ projectId, name, engineId, start: { kind: 'session', session: { kind: 'run', runId: past.run_id } } },
				ACTOR
			);
			showSeatNotice(
				`Seat @${name} created with session ${seatSessionNumberText(past)} — it leaves @${
					start.kind === 'resume' ? start.from.name : ''
				}’s history and resumes on the first dispatch · scratchpad ${r.seat.address}`
			);
			return r.seat;
		}
		const created = await seatsCreate({ projectId, name, engineId, start: { kind: 'empty' } }, ACTOR);
		if (!req.hasWrap) {
			showSeatNotice(`Seat @${name} created — it fills on its first dispatch · scratchpad ${created.seat.address}`);
			return created.seat;
		}
		try {
			const r = await occupyVacantSeat(created.seat.id, start.kind === 'resume' ? 'resume' : 'fill', {
				from: past,
			});
			showSeatNotice(
				start.kind === 'resume'
					? `Seat @${name} created resuming session ${seatSessionNumberText(past)} (it leaves @${start.from.name}’s history) · scratchpad ${r.seat.address}`
					: `Seat @${name} created with ${sessionName(r.terminalId)} · scratchpad ${r.seat.address}`
			);
			return r.seat;
		} catch (err) {
			// The seat exists; only its first session failed. Say so, keep the seat.
			showSeatNotice(`Seat @${name} created, vacant — ${errorText(err)}`, { variant: 'error' });
			return created.seat;
		}
	} catch (err) {
		throw new Error(errorText(err));
	} finally {
		void invalidateSeats(projectId);
	}
}

// ─── ⌥↑ / ⌥↓ (D-09 rule 5) ─────────────────────────────────────────────────

/** Same target, by identity. */
export function sameTarget(a: CompanionTarget, b: CompanionTarget): boolean {
	if (a.kind === 'session' && b.kind === 'session') return a.session_id === b.session_id;
	if (a.kind === 'seat' && b.kind === 'seat') return a.seat_id === b.seat_id;
	if (a.kind === 'new' && b.kind === 'new') return a.engine_id === b.engine_id;
	if (a.kind === 'persistent' && b.kind === 'persistent') return a.engine_id === b.engine_id;
	return false;
}

/** The target after `current` in `list`, cycling (`dir` ±1). */
export function cycleIn(list: readonly CompanionTarget[], current: CompanionTarget, dir: 1 | -1): CompanionTarget | null {
	if (list.length === 0) return null;
	const i = list.findIndex((t) => sameTarget(t, current));
	return list[(i + dir + list.length) % list.length] ?? null;
}

/** Apply a target from the picker / the cycle: seats and sessions select the
 *  rail row too; *New session on…* / *Persistent run* move only the chip. */
export function applyTarget(t: CompanionTarget): void {
	if (t.kind === 'seat') {
		const projectId = useShellStore.getState().activeProject.id;
		const seat = cachedSeats(projectId)?.find((s) => s.id === t.seat_id);
		if (seat) selectSeat(seat);
		else useShellStore.getState().setCompanionTarget(t);
		return;
	}
	if (t.kind === 'session') {
		selectSession(t.session_id);
		return;
	}
	useShellStore.getState().setCompanionTarget(t);
}
