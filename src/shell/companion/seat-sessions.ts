// WP-67 — the UI's session numbering ("session 3") and the per-session
// figures the rail, the scoped cost line and the status bar read.
//
// Numbering (D-09: `@lead · claude · session 3`). WP-66 labelled a session
// by the first 8 characters of its id in the §6.3 toasts; the locked rail
// numbers them. A number is assigned the first time a session is seen and
// kept for the app's lifetime, so it never shifts when another session
// closes. Terminals already in the store are numbered by `createdAt` before
// any newcomer, so the numbers read in the order the sessions were opened.
// A terminal or run that resumes an earlier conversation takes that
// conversation's number (`aliasSessionNumber`), not a new one.
//
// Figures (D-09 revision 2): only what an engine reported — the Claude
// statusline snapshot (`statusline://snapshot`, the feed `CostHud` reads).
// Anything missing is `null`, which the UI renders "—" with `UNREPORTED`.

import { useEffect } from 'react';
import { create } from 'zustand';
import { fetchStatuslineSnapshots } from '@/lib/iyke/terminal-hooks';
import type { SeatSession } from '@/lib/tauri-cmd';
import { listen } from '@/lib/transport';
import type { StatuslineSnapshot } from '@/terminal/cost-hud';
import { useTerminalStore } from '@/terminal/session-store';
import { ctxK, usd } from './seat-model';

// ─── Numbering ──────────────────────────────────────────────────────────────

const numbers = new Map<string, number>();
let next = 1;

function seedFromTerminals(): void {
	const tabs = [...useTerminalStore.getState().tabs].sort((a, b) => a.createdAt - b.createdAt);
	for (const tab of tabs) {
		if (!numbers.has(tab.id)) numbers.set(tab.id, next++);
	}
}

/** The UI number of a session (a terminal id or a run id). Stable once given. */
export function sessionNumber(ref: string): number {
	const known = numbers.get(ref);
	if (known !== undefined) return known;
	seedFromTerminals();
	const seeded = numbers.get(ref);
	if (seeded !== undefined) return seeded;
	const n = next++;
	numbers.set(ref, n);
	return n;
}

/**
 * A resumed conversation keeps its number: `ref` (the new terminal or run
 * that resumed it) takes `previousRef`'s number (D-09: *Resume session 2*
 * leaves the seat reading "session 2"). Overwrites any number `ref` got.
 */
export function aliasSessionNumber(ref: string, previousRef: string): void {
	if (ref === previousRef) return;
	numbers.set(ref, sessionNumber(previousRef));
}

/** `session 3`. */
export function sessionName(ref: string): string {
	return `session ${sessionNumber(ref)}`;
}

/** The §6.3 `<N>` for a seat's session: its number, or "—" when there is none. */
export function seatSessionNumberText(session: SeatSession | null | undefined): string {
	if (!session) return '—';
	return String(sessionNumber(session.kind === 'terminal' ? session.terminal_id : session.run_id));
}

/** Test seam. */
export function __resetSessionNumbersForTests(): void {
	numbers.clear();
	next = 1;
}

// ─── Figures ────────────────────────────────────────────────────────────────

interface FiguresState {
	snaps: Record<string, StatuslineSnapshot>;
}

export const useSessionFiguresStore = create<FiguresState>(() => ({ snaps: {} }));

let feedStarted = false;

/** Subscribe once (idempotent) to the statusline feed the figures come from. */
function ensureFiguresFeed(): void {
	if (feedStarted) return;
	feedStarted = true;
	fetchStatuslineSnapshots<StatuslineSnapshot>()
		.then((data) => {
			if (data && typeof data === 'object') {
				useSessionFiguresStore.setState((s) => ({ snaps: { ...data, ...s.snaps } }));
			}
		})
		.catch(() => {});
	listen<StatuslineSnapshot>('statusline://snapshot', (event) => {
		const id = event.payload?.ikenga_terminal_id;
		if (!id) return;
		useSessionFiguresStore.setState((s) => ({ snaps: { ...s.snaps, [id]: event.payload } }));
	}).catch(() => {
		feedStarted = false;
	});
}

export interface SessionFigures {
	/** `$1.42`, or null when not reported. */
	amt: string | null;
	/** `38k`, or null when not reported. */
	ctx: string | null;
}

export function figuresOf(snap: StatuslineSnapshot | undefined): SessionFigures {
	return {
		amt: usd(snap?.cost?.total_cost_usd),
		ctx: ctxK(snap?.context_window?.total_input_tokens),
	};
}

/** The reported figures of one session (`null` ref → nothing reported). */
export function useSessionFigures(ref: string | null): SessionFigures {
	useEffect(() => {
		ensureFiguresFeed();
	}, []);
	const snap = useSessionFiguresStore((s) => (ref ? s.snaps[ref] : undefined));
	return figuresOf(snap);
}

/** Test seam. */
export function __resetFiguresFeedForTests(): void {
	feedStarted = false;
	useSessionFiguresStore.setState({ snaps: {} });
}
