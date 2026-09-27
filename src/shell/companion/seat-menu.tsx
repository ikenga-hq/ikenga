// WP-67 — the seat `⋯` / right-click menu and the unseated-session menu
// (D-09 `seats-companion.html` `seatMenu` / `sessMenu`, G-SEATS §4.4, §5.5).
//
// Item order is the locked file's: Open in pane · Make dispatch target ·
// Open scratchpad · Pop out · All seats │ Rename… · Copy address · Copy as
// iyke │ End session · Remove seat…. *Take over* (Round 45, §5.5) is added
// only while another client holds the seat, so the resting menu is D-09's.
//
// **Pop out / Open in pane** (G-97, Round 50): this file holds their call
// sites. WP-69 (DEC-69d, G-SEATS §4.4) made Pop out join "Window 2" — the
// most recently focused live secondary window — and spawn one only when
// none is open, and gave a persistent-run seat something to show: a
// terminal attached to its tmux session (`terminal/attach-run.ts`). A
// one-off run seat stays disabled ("headless run — nothing to show").
// Nothing here touches the seat row: its address and dispatch are
// unchanged (D-09 rule 2).

import { useEffect, useRef } from 'react';
import { cn } from '@/components/ui/utils';
import { getLeafIdsInOrder } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import { cachedSeats } from '@/lib/queries/seats';
import { activeProjectCwd } from '@/lib/shell/active-project-cwd';
import { useShellStore } from '@/lib/shell/shell-store';
import type { SeatView } from '@/lib/tauri-cmd';
import { onMakeTargetRequested, onSurfacesReturned, type SurfacesReturned } from '@/lib/window/detached-surfaces';
import { isDetachedWindow } from '@/lib/window/window-context';
import { forgetWindowTwoLabel, isWindowTwoLabel, popOutSurface } from '@/lib/window/window-two';
import { attachRunTerminal, isRunAttachCmd, type RunAttachState, runSessionOf } from '@/terminal/attach-run';
import { type TerminalTab, useTerminalStore } from '@/terminal/session-store';
import { makeTarget, mountOfTerminal, openSessionInPane } from './seat-actions';
import { seatSessionRef } from './seat-model';
import { showSeatNotice } from './seat-notice';
import { sessionName } from './seat-sessions';

// ─── Pop out / Open in pane (the call sites, G-97 + Round 50) ──────────────

/** D-09's disabled reason for a one-off run seat (§4.4). */
export const HEADLESS_RUN_REASON = 'Headless run — nothing to show';

/**
 * Pop a terminal out to Window 2: join it when one is open, else spawn it
 * (DEC-69d). The seat row never changes: the address is not the mount
 * (D-09 rule 2, §4.4). `name` is the seat's name, or the session's label
 * for an unseated one.
 *
 * The terminal's tab stays in its main-window pane as the "popped out"
 * placeholder rather than being removed as D-09's `popOut` does: closing a
 * pane tab releases its terminal (`pane-store.releaseAttachments` kills the
 * PTY once no view references it). The iyke snapshot leaves detached
 * placeholders out of the terminal's mount (`use-iyke-shell-sync`), so Rust
 * still reads the seat as mounted in Window 2 (§2.1 `popped-out`).
 */
export function popOutTerminal(terminalId: string, name: string): void {
	const tab = useTerminalStore.getState().tabs.find((t) => t.id === terminalId);
	const ptyId = tab?.ptyId;
	if (!ptyId || tab?.status !== 'running') {
		showSeatNotice(`${name}’s terminal isn’t running — nothing to pop out`, { variant: 'error' });
		return;
	}
	ensureReturnNotices();
	popOutSurface(`terminal:${ptyId}`, {
		projectId: useShellStore.getState().activeProject.id,
		kind: 'terminal',
	})
		.then(() => showSeatNotice(`${name} moved to Window 2 — its address is unchanged`))
		.catch((err: unknown) => {
			showSeatNotice(`Couldn’t pop out ${name}: ${err instanceof Error ? err.message : String(err)}`, {
				variant: 'error',
			});
		});
}

/**
 * Why a seat's *Open in pane* / *Pop out* is disabled, or `''` when it can
 * run (§4.4). `live` is whether the seat's terminal is running (for a run
 * seat: its attached terminal, which doesn't matter here). `run` is a run
 * seat's attach state from `useRunAttachedTerminal` (`undefined` while it
 * loads) — passed in, never read from the cache here, since this runs
 * during render.
 */
export function seatPaneBlocker(seat: SeatView, live: boolean, run?: RunAttachState): string {
	if (seat.status === 'vacant' || !seat.session) return 'Vacant — resume or fill it first';
	if (seat.session.kind === 'terminal') return live ? '' : 'Its terminal isn’t running';
	// A run seat: only a persistent run in flight has a tmux session to attach.
	if (run?.kind === 'headless') return HEADLESS_RUN_REASON;
	if (seat.status !== 'run') return 'The run has finished — nothing to attach to';
	if (run === undefined) return 'Checking the run…';
	if (run.kind === 'pending') return 'The run hasn’t started yet — nothing to attach to';
	if (run.kind === 'unknown') return 'Couldn’t find the run among recent runs';
	return '';
}

/** The seat's terminal — for a persistent run, one attached to its tmux
 *  session (reused when one is running, else spawned now). */
async function seatTerminal(seat: SeatView, run: RunAttachState | undefined): Promise<string> {
	const s = seat.session;
	if (!s) throw new Error('the seat is vacant');
	if (s.kind === 'terminal') return s.terminal_id;
	const session = runSessionOf(run);
	if (!session) throw new Error('headless run — nothing to show');
	return attachRunTerminal({ session, cwd: s.cwd ?? activeProjectCwd(), title: `${seat.name} · run` });
}

/** *Pop out* on a seat (§4.4). `run`: a run seat's attach state. */
export function popOutSeat(seat: SeatView, run?: RunAttachState): void {
	void seatTerminal(seat, run)
		.then((terminalId) => popOutTerminal(terminalId, seat.name))
		.catch((err: unknown) =>
			showSeatNotice(`Couldn’t pop out ${seat.name}: ${err instanceof Error ? err.message : String(err)}`, {
				variant: 'error',
			})
		);
}

/** *Open in pane* on a seat (§4.4): brings it back from Window 2, focuses
 *  the pane holding it, or opens it in the focused pane. `run`: a run
 *  seat's attach state. */
export function openSeatInPane(seat: SeatView, run?: RunAttachState): void {
	void seatTerminal(seat, run)
		.then((terminalId) => openSessionInPane(terminalId))
		.catch((err: unknown) =>
			showSeatNotice(`Couldn’t open ${seat.name}: ${err instanceof Error ? err.message : String(err)}`, {
				variant: 'error',
			})
		);
}

// ─── Coming back from Window 2 (D-09 `moveBack` / `closeWin2`) ──────────────

/** The seat whose session is `terminalId` (its own terminal, or a tmux
 *  client attached to its run), if any. */
function seatOfTerminal(terminalId: string): SeatView | undefined {
	const seats = cachedSeats(useShellStore.getState().activeProject.id) ?? [];
	const tab = useTerminalStore.getState().tabs.find((t) => t.id === terminalId);
	return seats.find(
		(st) =>
			(st.session?.kind === 'terminal' && st.session.terminal_id === terminalId) ||
			(st.session?.kind === 'run' && tab && isRunAttachCmd(tab.spec.cmd, st.session.run_id))
	);
}

/** What a toast calls terminal `terminalId` (D-09 `popOut` / `moveBack`):
 *  its seat's name, else its session label. Read from the roster cache only,
 *  so a Pop out never waits on a fetch. The pane's own Pop out uses it too. */
export function terminalToastName(terminalId: string): string {
	const seat = seatOfTerminal(terminalId);
	return seat ? seat.name : sessionName(terminalId);
}

/** D-09 `mountOf().long` for a main-window pane: "main window · pane N of M". */
function mainPaneText(terminal: TerminalTab): string {
	const root = usePaneStore.getState().root;
	const m = mountOfTerminal(terminal.id, root, {}, terminal.ptyId);
	return m.where === 'main'
		? `main window · pane ${m.paneIndex} of ${getLeafIdsInOrder(root).length}`
		: 'the main window';
}

/**
 * Surfaces that left a detached window come back into the main window.
 *
 * A terminal a main-window pane still holds (its "popped out" placeholder —
 * every pane pop-out, and a seat popped out from a pane) just shows live
 * there again; a Move back also brings that pane and tab forward. Only a
 * terminal no pane holds (a seat popped out straight from the rail) is
 * re-homed, as a tab in the focused pane — never a second tab for the same
 * terminal.
 *
 * The toast speaks D-09's words, and only for Window 2: a closed window
 * speaks only when a Pop out put something in it (`isWindowTwoLabel`), so
 * closing an ordinary pane / viewer pop-out stays silent as it always was,
 * and it counts every surface brought back (terminals and viewers).
 */
export function handleSurfacesReturned(e: SurfacesReturned): void {
	const terminals = e.surfaceIds
		.filter((id) => id.startsWith('terminal:'))
		.map((id) => useTerminalStore.getState().tabs.find((t) => t.ptyId === id.slice('terminal:'.length)))
		.filter((t): t is NonNullable<typeof t> => Boolean(t));
	// Anything else (a viewer) was popped out from a pane, whose placeholder
	// shows it live again: it came back too, so the count includes it.
	let returned = e.surfaceIds.filter((id) => !id.startsWith('terminal:')).length;
	for (const t of terminals) {
		const panes = usePaneStore.getState();
		const view = { kind: 'terminal' as const, sessionId: t.id };
		const m = mountOfTerminal(t.id, panes.root, {}, t.ptyId);
		if (m.where === 'none') {
			panes.addTab(panes.focusedId, view);
		} else if (m.where === 'main' && e.reason === 'move-back') {
			// Already held: switch to its tab and focus that pane (no new tab).
			panes.placeView(m.leafId, view, 'append');
		}
		returned += 1;
	}
	if (e.reason === 'window-closed') {
		const windowTwo = isWindowTwoLabel(e.label);
		forgetWindowTwoLabel(e.label);
		if (!windowTwo || returned === 0) return;
		showSeatNotice(
			`Window 2 closed — ${returned} pane${returned === 1 ? '' : 's'} returned to the main window; addresses unchanged`
		);
		return;
	}
	const t = terminals[0];
	if (!t) return;
	showSeatNotice(`${terminalToastName(t.id)} moved to ${mainPaneText(t)} — its address is unchanged`);
}

/**
 * Window 2 ⋯ → *Make dispatch target* (D-09), asked by the thin window over
 * `window://make-target`: select the surface's seat, or its session when it
 * has none (selection ≡ target).
 */
export function handleMakeTargetRequest(surfaceId: string): void {
	if (!surfaceId.startsWith('terminal:')) return;
	const ptyId = surfaceId.slice('terminal:'.length);
	const tab = useTerminalStore.getState().tabs.find((t) => t.ptyId === ptyId);
	if (!tab) return;
	const seat = seatOfTerminal(tab.id);
	if (seat) makeTarget({ kind: 'seat', seat_id: seat.id }, seatSessionRef(seat));
	else makeTarget({ kind: 'session', session_id: tab.id }, tab.id);
}

let returnNoticesOn = false;

/** Subscribe the Companion to surfaces coming back and to Window 2's
 *  *Make dispatch target* (once, primary only). */
export function ensureReturnNotices(): void {
	if (returnNoticesOn || isDetachedWindow()) return;
	returnNoticesOn = true;
	onSurfacesReturned(handleSurfacesReturned);
	onMakeTargetRequested(handleMakeTargetRequest);
}

// The rail imports this module at boot, so returns are handled even for a
// window popped out from a pane before any seat was.
ensureReturnNotices();

// ─── The menu ───────────────────────────────────────────────────────────────

export type SeatMenuItem =
	| { sep: true }
	| {
			sep?: false;
			label: string;
			/** Trailing muted text (`seat:royalti-co/lead`, `F2`). */
			sub?: string;
			disabled?: boolean;
			/** Why it is disabled, or what it does. */
			title?: string;
			danger?: boolean;
			run: () => void;
	  };

export function SeatMenu({
	label,
	x,
	y,
	items,
	onClose,
}: {
	/** Accessible name, e.g. "Seat actions for @lead". */
	label: string;
	x: number;
	y: number;
	items: SeatMenuItem[];
	/** Close; `restoreFocus` is false when the click landed elsewhere. */
	onClose: (restoreFocus: boolean) => void;
}) {
	const ref = useRef<HTMLDivElement | null>(null);
	// Read through a ref: the rail re-renders on every roster refetch, and a
	// fresh `onClose` must not re-run the mount effect (it would steal focus
	// back to the first item).
	const closeRef = useRef(onClose);
	closeRef.current = onClose;

	useEffect(() => {
		ref.current?.querySelector<HTMLElement>('[role="menuitem"]:not([disabled])')?.focus();
		const onDown = (e: MouseEvent) => {
			if (!ref.current?.contains(e.target as Node)) closeRef.current(false);
		};
		window.addEventListener('mousedown', onDown);
		return () => window.removeEventListener('mousedown', onDown);
	}, []);

	function onKeyDown(e: React.KeyboardEvent) {
		const enabled = Array.from(
			ref.current?.querySelectorAll<HTMLElement>('[role="menuitem"]:not([disabled])') ?? []
		);
		const at = enabled.indexOf(document.activeElement as HTMLElement);
		if (e.key === 'Escape') {
			e.preventDefault();
			e.stopPropagation();
			onClose(true);
		} else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
			e.preventDefault();
			const d = e.key === 'ArrowDown' ? 1 : -1;
			enabled[(at + d + enabled.length) % enabled.length]?.focus();
		} else if (e.key === 'Home') {
			e.preventDefault();
			enabled[0]?.focus();
		} else if (e.key === 'End') {
			e.preventDefault();
			enabled.at(-1)?.focus();
		} else if (e.key === 'Tab') {
			e.preventDefault();
			onClose(true);
		}
	}

	// Keep the menu inside the viewport (it opens at the pointer or the ⋯).
	const vw = typeof window !== 'undefined' ? window.innerWidth : 1440;
	const vh = typeof window !== 'undefined' ? window.innerHeight : 900;
	const left = Math.max(4, Math.min(x, vw - 244));
	const top = Math.max(4, Math.min(y, vh - (items.length * 26 + 16)));

	return (
		<div
			ref={ref}
			role="menu"
			aria-label={label}
			onKeyDown={onKeyDown}
			className="fixed z-50 w-60 rounded-md border py-1 shadow-lg"
			style={{ left, top, background: 'var(--bg-raised)', borderColor: 'var(--border)' }}
		>
			{items.map((item, i) =>
				item.sep ? (
					<div
						// biome-ignore lint/suspicious/noArrayIndexKey: separators have no identity
						key={`sep-${i}`}
						role="separator"
						className="my-1 h-px"
						style={{ background: 'var(--border-soft)' }}
					/>
				) : (
					<button
						key={item.label}
						type="button"
						role="menuitem"
						tabIndex={-1}
						disabled={item.disabled}
						title={item.title || undefined}
						onClick={() => {
							onClose(true);
							item.run();
						}}
						className={cn(
							'flex min-h-6 w-full items-center gap-2 px-3 py-1 text-left text-xs',
							'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring',
							'disabled:cursor-not-allowed disabled:opacity-60 enabled:hover:bg-[var(--bg-sunken)]',
							item.danger ? 'text-[var(--color-text-danger)]' : 'text-[var(--fg)]'
						)}
					>
						<span className="truncate">{item.label}</span>
						{item.sub && (
							<span
								className="ml-auto truncate pl-2 font-mono text-[11px]"
								style={{ color: 'var(--fg-muted)' }}
							>
								{item.sub}
							</span>
						)}
					</button>
				)
			)}
		</div>
	);
}
