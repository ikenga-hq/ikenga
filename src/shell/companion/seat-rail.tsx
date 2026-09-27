// WP-67 — the seat rail (D-09 `seats-companion.html`, the primary seat
// surface, DEC-66). It replaces the session tabs (spec §3.11 #66): seat rows
// directly under the dispatch bar, then an "Unseated" group, then the iyke
// form of what ↵ would do.
//
//   click        select — targets the seat AND scopes the permission, cost
//                and tool-feed panels to its session (G-SEATS §9.1)
//   dbl / ↵      open its terminal in a pane
//   drag         onto a pane: centre mounts it as a tab, an edge splits
//                (the gesture ported from D-09's Explorer variant)
//   right / ⋯    the seat menu (right-click never selects: selecting would
//                also retarget dispatch)
//   ↑ ↓ Home End rove + select (one tab stop, roving tabindex)
//   F2 rename · Delete remove (confirms) · Shift+F10 / ContextMenu menu
//
// ADR-021: rows carry state and addresses only — never model output.

import {
	AppWindow,
	Copy,
	FileEdit,
	Inbox,
	LayoutGrid,
	MoreHorizontal,
	Plus,
	ShieldAlert,
} from 'lucide-react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { cn } from '@/components/ui/utils';
import { useDragState } from '@/lib/panes/drag-state';
import { usePaneStore } from '@/lib/panes/pane-store';
import { beginPointerDrag } from '@/lib/panes/pointer-drag';
import { UI_SEAT_CLIENT } from '@/lib/queries/seats';
import { type CompanionTarget, useShellStore } from '@/lib/shell/shell-store';
import type { SeatStatus, SeatView } from '@/lib/tauri-cmd';
import { useDetachedSurfaces } from '@/lib/window/detached-surfaces';
import { type RunAttachState, useRunAttachedTerminal } from '@/terminal/attach-run';
import { useTerminalStore } from '@/terminal/session-store';
import { type RailSelection, useCompanionStore } from './companion-store';
import {
	cancelRename,
	copyText,
	endSeatSession,
	endUnseatedSession,
	makeTarget,
	type Mount,
	mountOfTerminal,
	mountTerminalAt,
	openSeatBoard,
	openSeatForm,
	openSeatScratchpad,
	openSessionInPane,
	releaseSeat,
	renameSeat,
	selectSeat,
	selectSession,
	startRename,
	takeOverSeat,
	useSeatUi,
} from './seat-actions';
import {
	openSeatInPane,
	popOutSeat,
	popOutTerminal,
	SeatMenu,
	type SeatMenuItem,
	seatPaneBlocker,
} from './seat-menu';
import {
	atName,
	checkSeatName,
	engineResumeFlag,
	engineShort,
	heldByOther,
	holdText,
	iykeChiRun,
	iykeSendToSeat,
	iykeSendToTerminal,
	padText,
	seatSessionRef,
	seatWhoLine,
	stateDotColor,
	UNREPORTED,
} from './seat-model';
import { formatSeatTime } from './seat-notice';
import type { SeatRoster, UnseatedSession } from './seat-roster';
import { sessionName, useSessionFigures } from './seat-sessions';

// ─── Selection ↔ target sync ────────────────────────────────────────────────

/**
 * Keeps the rail selection, the dispatch target and the panel scope one
 * choice (§9.1) when something outside the rail moves one of them:
 * - a target set elsewhere (the Explorer's *Make dispatch target*, a pane
 *   drop onto the Companion) selects its row — a session a seat holds
 *   selects the seat (D-09: "speaks in the seat");
 * - a selected seat whose session changed (resumed, filled, moved) re-scopes
 *   the panels to the new session.
 * Mounted by the Companion in every state.
 */
export function useRailSync(roster: SeatRoster): void {
	const target = useShellStore((s) => s.companion.activeTarget);
	const sel = useCompanionStore((s) => s.railSelection);
	const scope = useCompanionStore((s) => s.panelScopeSessionId);
	const { seats, state } = roster;
	useEffect(() => {
		const store = useCompanionStore.getState();
		if (target.kind === 'seat') {
			const seat = seats.find((s) => s.id === target.seat_id);
			if (!seat) return;
			const ref = seatSessionRef(seat);
			if (sel?.kind !== 'seat' || sel.seat_id !== seat.id) store.selectRail({ kind: 'seat', seat_id: seat.id }, ref);
			else if (ref !== scope) store.setPanelScope(ref);
			return;
		}
		if (target.kind === 'session') {
			const holder = seats.find(
				(s) => s.session?.kind === 'terminal' && s.session.terminal_id === target.session_id
			);
			if (holder && state === 'ready') {
				store.selectRail({ kind: 'seat', seat_id: holder.id }, target.session_id);
				return;
			}
			if (sel?.kind !== 'session' || sel.session_id !== target.session_id) {
				useCompanionStore.setState({
					railSelection: { kind: 'session', session_id: target.session_id },
					panelScopeSessionId: target.session_id,
				});
			}
		}
	}, [target, sel, scope, seats, state]);
}

// ─── Small parts ────────────────────────────────────────────────────────────

export function StateDot({ status, className }: { status: SeatStatus; className?: string }) {
	return (
		<span
			aria-hidden="true"
			data-status={status}
			className={cn(
				'size-2 shrink-0 rounded-full',
				status === 'run' && 'motion-safe:animate-pulse',
				className
			)}
			style={{
				background: stateDotColor(status),
				boxShadow: status === 'vacant' ? 'inset 0 0 0 1.5px var(--fg-muted)' : undefined,
			}}
		/>
	);
}

/** A row signal: one true thing, hidden at zero. */
function Signal({
	tone = 'plain',
	icon,
	title,
	children,
}: {
	tone?: 'plain' | 'ask' | 'inbox' | 'window' | 'hold';
	icon?: React.ReactNode;
	title: string;
	children: React.ReactNode;
}) {
	return (
		<span
			title={title}
			data-signal={tone}
			className="inline-flex h-[18px] shrink-0 items-center gap-[3px] whitespace-nowrap rounded-full border px-[5px] font-mono text-[11px] [&_svg]:size-[11px]"
			style={
				tone === 'ask'
					? {
							color: 'var(--on-achievement, var(--achievement))',
							background: 'var(--achievement-soft)',
							borderColor: 'transparent',
						}
					: tone === 'inbox'
						? { color: 'var(--on-info, var(--info))', background: 'var(--info-soft)', borderColor: 'transparent' }
						: tone === 'window'
							? { color: 'var(--fg)', background: 'var(--bg-raised)', borderColor: 'var(--border-strong)' }
							: { color: 'var(--fg-muted)', background: 'var(--bg-raised)', borderColor: 'var(--border-soft)' }
			}
		>
			{icon}
			<span>{children}</span>
		</span>
	);
}

/** The mount readout (never part of the address, D-09 rule 2). */
function mountSignal(mount: Mount, isRun: boolean) {
	if (mount.where === 'window') {
		return (
			<Signal tone="window" icon={<AppWindow aria-hidden="true" />} title="Popped out to a second window · the address is unchanged">
				Window 2
			</Signal>
		);
	}
	if (mount.where === 'main') {
		return <Signal title={`main window · pane ${mount.paneIndex}`}>{`pane ${mount.paneIndex}`}</Signal>;
	}
	if (isRun) return <Signal title="Headless run — no pane; dispatch still reaches it">headless</Signal>;
	return <Signal title="Live, in no pane — dispatch still reaches it">not mounted</Signal>;
}

function useMount(terminalId: string | null): Mount {
	const root = usePaneStore((s) => s.root);
	const detached = useDetachedSurfaces((s) => s.surfaceToWindow);
	const ptyId = useTerminalStore((s) => (terminalId ? (s.tabs.find((t) => t.id === terminalId)?.ptyId ?? null) : null));
	return useMemo(
		() => (terminalId ? mountOfTerminal(terminalId, root, detached, ptyId) : { where: 'none' as const }),
		[terminalId, root, detached, ptyId]
	);
}

/** Whether a terminal id is a live PTY we can mount or pop out. */
function useTerminalLive(terminalId: string | null): boolean {
	return useTerminalStore((s) =>
		terminalId ? s.tabs.some((t) => t.id === terminalId && t.status === 'running') : false
	);
}

function RailIykeLine({ cmd }: { cmd: string }) {
	return (
		<div
			className="flex h-7 shrink-0 items-center gap-2 border-t px-3 font-mono text-[11px]"
			style={{ borderColor: 'var(--border-soft)', color: 'var(--fg-muted)' }}
		>
			<b className="font-semibold" style={{ color: 'var(--fg)' }}>
				iyke
			</b>
			<span data-iyke-line="" className="min-w-0 flex-1 truncate" style={{ color: 'var(--fg)' }} title={`iyke ${cmd}`}>
				{cmd}
			</span>
			<button
				type="button"
				onClick={() => copyText(`iyke ${cmd}`, `Copied iyke ${cmd}`)}
				aria-label="Copy the iyke command"
				className="inline-flex shrink-0 items-center gap-1 rounded-sm px-1 hover:text-[var(--fg)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
			>
				<Copy className="h-3 w-3" aria-hidden="true" />
				Copy
			</button>
		</div>
	);
}

/** The iyke call ↵ would make right now, with the draft in it (Principle 5). */
export function iykeForTarget(target: CompanionTarget, seats: readonly SeatView[], draft: string, defaultEngine: string | null): string {
	switch (target.kind) {
		case 'seat': {
			const seat = seats.find((s) => s.id === target.seat_id);
			return iykeSendToSeat(seat?.name ?? '<seat>', draft);
		}
		case 'session':
			return iykeSendToTerminal(target.session_id, draft);
		case 'new':
			return iykeChiRun(target.engine_id ?? defaultEngine ?? '<engine>', false, draft);
		case 'persistent':
			return iykeChiRun(target.engine_id ?? defaultEngine ?? '<engine>', true, draft);
	}
}

// ─── Rows ───────────────────────────────────────────────────────────────────

type MenuState =
	| { kind: 'seat'; seatId: string; x: number; y: number }
	| { kind: 'session'; id: string; x: number; y: number };

function rowStyle(selected: boolean): React.CSSProperties {
	return selected
		? { background: 'var(--tint-bg-active)', boxShadow: 'inset 2px 0 0 var(--primary)' }
		: {};
}

const ROW_CLASS =
	'relative flex flex-col justify-center pl-3 pr-2 text-[var(--fg-muted)] hover:bg-[var(--bg-raised)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring';

/** F2 / *Rename…* inline: live §1.2 validation; ↵ renames, Esc or a click
 *  elsewhere cancels. `onMessage` reports the line shown under the row. */
function RenameField({
	seat,
	taken,
	removing,
	onMessage,
}: {
	seat: SeatView;
	taken: string[];
	removing: string[];
	onMessage: (message: string) => void;
}) {
	const [value, setValue] = useState(seat.name);
	const [hostError, setHostError] = useState<string | null>(null);
	const ref = useRef<HTMLInputElement | null>(null);
	const check = checkSeatName(value, taken, { except: seat.name, removing });
	useEffect(() => {
		ref.current?.focus();
		ref.current?.select();
	}, []);
	const shown = hostError ?? (check.ok || check.empty ? '' : check.message);
	useEffect(() => onMessage(shown), [shown, onMessage]);
	return (
		<input
			ref={ref}
			type="text"
			value={value}
			data-seat-name-field=""
			aria-label={`New name for @${seat.name}`}
			aria-invalid={Boolean(shown) || undefined}
			spellCheck={false}
			autoComplete="off"
			onChange={(e) => {
				setValue(e.target.value);
				setHostError(null);
			}}
			onKeyDown={(e) => {
				e.stopPropagation();
				if (e.key === 'Escape') {
					e.preventDefault();
					cancelRename();
				} else if (e.key === 'Enter') {
					e.preventDefault();
					const next = value.trim();
					if (next === seat.name) return cancelRename();
					if (!check.ok) return;
					void renameSeat(seat, next).then((err) => {
						if (err) setHostError(err);
					});
				}
			}}
			onBlur={() => {
				// A click elsewhere cancels, like Esc (the name only changes on ↵).
				setTimeout(() => {
					if (useSeatUi.getState().renaming === seat.id && document.activeElement !== ref.current) cancelRename();
				}, 0);
			}}
			className="h-[22px] min-w-0 flex-1 rounded-[var(--radius-xs)] border bg-[var(--bg-sunken)] px-1 font-mono text-[13px] text-[var(--fg)] outline-none"
			style={{ borderColor: shown ? 'var(--danger)' : 'var(--primary)' }}
		/>
	);
}

function SeatRow({
	seat,
	selected,
	focusable,
	renaming,
	takenNames,
	removingNames,
	pending,
	onMenu,
}: {
	seat: SeatView;
	selected: boolean;
	focusable: boolean;
	renaming: boolean;
	takenNames: string[];
	removingNames: string[];
	pending: number;
	onMenu: (seat: SeatView, x: number, y: number) => void;
}) {
	const terminalId = seat.session?.kind === 'terminal' ? seat.session.terminal_id : null;
	const ref = seatSessionRef(seat);
	const mount = useMount(seat.status === 'vacant' ? null : terminalId);
	const live = useTerminalLive(terminalId);
	const figures = useSessionFigures(ref);
	const name = ref ? sessionName(ref) : null;
	const who = seatWhoLine(seat, name);
	const flag = engineResumeFlag(seat.engine_resume);
	const held = holdText(seat.hold, formatSeatTime);
	const stateWord =
		seat.status === 'live'
			? `live · ${figures.ctx ?? '—'} ctx`
			: seat.status === 'idle'
				? 'idle'
				: seat.status === 'run'
					? 'run'
					: 'vacant';
	const canMount = seat.status !== 'vacant' && live && terminalId !== null;
	const [renameMsg, setRenameMsg] = useState('');

	return (
		<div
			role="option"
			aria-selected={selected}
			tabIndex={focusable ? 0 : -1}
			id={`seat-row-${seat.id}`}
			data-seat={seat.id}
			data-status={seat.status}
			aria-label={`${atName(seat.name)}, ${stateWord}${pending ? `, ${pending} permission${pending === 1 ? '' : 's'} pending` : ''}${held ? `, ${held}` : ''}`}
			onClick={(e) => {
				if ((e.target as HTMLElement).closest('button, input')) return;
				if (!selected) selectSeat(seat);
			}}
			onDoubleClick={(e) => {
				if ((e.target as HTMLElement).closest('button, input')) return;
				if (terminalId && seat.status !== 'vacant') openSessionInPane(terminalId);
			}}
			onContextMenu={(e) => {
				e.preventDefault();
				onMenu(seat, e.clientX, e.clientY);
			}}
			onPointerDown={
				canMount && terminalId && !renaming
					? (e) => {
							if ((e.target as HTMLElement).closest('button, input')) return;
							beginPointerDrag(e, {
								label: atName(seat.name),
								onStart: () =>
									useDragState
										.getState()
										.startExternal((paneId, mode) => mountTerminalAt(terminalId, paneId, mode, seat.name)),
								onEnd: () => useDragState.getState().end(),
							});
						}
					: undefined
			}
			className={ROW_CLASS}
			style={rowStyle(selected)}
		>
			<div className="flex h-[30px] min-w-0 items-center gap-2">
				<StateDot status={seat.status} />
				{renaming ? (
					<RenameField seat={seat} taken={takenNames} removing={removingNames} onMessage={setRenameMsg} />
				) : (
					<>
						<span className="max-w-[104px] shrink-0 truncate font-mono text-[13px] font-medium" style={{ color: 'var(--fg)' }}>
							<span className="font-normal" style={{ color: 'var(--fg-muted)' }}>
								@
							</span>
							{seat.name}
						</span>
						<span className="min-w-0 flex-1 truncate text-[11px]">{who}</span>
					</>
				)}
				{!renaming && (
					<span className="ml-auto flex shrink-0 items-center gap-1">
						{pending > 0 && (
							<Signal tone="ask" icon={<ShieldAlert aria-hidden="true" />} title={`${pending} permission${pending === 1 ? '' : 's'} pending`}>
								{String(pending)}
							</Signal>
						)}
						{seat.inbox_count > 0 && seat.status !== 'vacant' && (
							<Signal tone="inbox" icon={<Inbox aria-hidden="true" />} title={`inbox: ${seat.inbox_count}`}>
								{String(seat.inbox_count)}
							</Signal>
						)}
						{seat.status !== 'vacant' && mountSignal(mount, seat.session?.kind === 'run')}
					</span>
				)}
			</div>
			{renaming && (
				<div role="status" data-rename-error="" className="pb-1 pl-[26px] text-[11px]" style={{ color: 'var(--color-text-danger)' }}>
					{renameMsg}
				</div>
			)}
			{(flag || held) && !renaming && (
				<div className="-mt-1 flex min-w-0 items-center gap-2 pb-1 pl-4 text-[11px]" data-seat-flags="">
					{held && (
						<span className="truncate" style={{ color: 'var(--on-achievement, var(--achievement))' }} title={held}>
							{held}
						</span>
					)}
					{flag && (
						<span className="truncate" style={{ color: 'var(--fg-muted)' }} title={flag}>
							{flag}
						</span>
					)}
				</div>
			)}
			{selected && !renaming && (
				<div className="-mt-[3px] flex min-w-0 items-center gap-2 pb-2 pl-[14px]">
					<button
						type="button"
						tabIndex={-1}
						onClick={() => openSeatScratchpad(seat)}
						title={`Open scratchpad ${seat.address}`}
						className="-ml-1 inline-flex h-5 min-w-0 items-center gap-1 rounded-[var(--radius-xs)] px-1 text-[11px] hover:bg-[var(--border-soft)] hover:text-[var(--fg)]"
					>
						<FileEdit className="h-3 w-3 shrink-0" aria-hidden="true" />
						<span className="shrink-0 font-mono" style={{ color: 'var(--fg)' }}>
							{seat.address}
						</span>
						<span className="min-w-0 truncate">{padText(seat)}</span>
					</button>
					<button
						type="button"
						tabIndex={-1}
						aria-haspopup="menu"
						aria-label={`Seat actions for @${seat.name}`}
						title="Seat actions (Shift+F10)"
						onClick={(e) => {
							const r = e.currentTarget.getBoundingClientRect();
							onMenu(seat, r.left, r.bottom + 4);
						}}
						className="ml-auto grid size-5 shrink-0 place-items-center rounded-sm hover:bg-[var(--bg-raised)] hover:text-[var(--fg)]"
					>
						<MoreHorizontal className="h-3.5 w-3.5" aria-hidden="true" />
					</button>
				</div>
			)}
		</div>
	);
}

function UnseatedRow({
	session,
	selected,
	focusable,
	pending,
	showSeatButton,
	onMenu,
}: {
	session: UnseatedSession;
	selected: boolean;
	focusable: boolean;
	pending: number;
	showSeatButton: boolean;
	onMenu: (id: string, x: number, y: number) => void;
}) {
	const mount = useMount(session.id);
	const figures = useSessionFigures(session.id);
	const live = session.status === 'running' || session.status === 'spawning';
	const status: SeatStatus = live ? 'live' : 'idle';
	const engine = session.engineId ? engineShort(session.engineId) : 'shell';
	const name = sessionName(session.id);
	const canSeat = session.engineId !== null && session.status === 'running';

	return (
		<div
			role="option"
			aria-selected={selected}
			tabIndex={focusable ? 0 : -1}
			data-session={session.id}
			aria-label={`${engine} · ${name}, unseated, ${live ? 'live' : 'idle'}`}
			onClick={(e) => {
				if ((e.target as HTMLElement).closest('button')) return;
				if (!selected) selectSession(session.id);
			}}
			onDoubleClick={(e) => {
				if ((e.target as HTMLElement).closest('button')) return;
				openSessionInPane(session.id);
			}}
			onContextMenu={(e) => {
				e.preventDefault();
				onMenu(session.id, e.clientX, e.clientY);
			}}
			onPointerDown={(e) => {
				if ((e.target as HTMLElement).closest('button')) return;
				const parked = session.parkedIdx;
				beginPointerDrag(e, {
					label: name,
					onStart: () =>
						parked !== null
							? useDragState.getState().startDock(parked)
							: useDragState
									.getState()
									.startExternal((paneId, mode) => mountTerminalAt(session.id, paneId, mode, null)),
					onEnd: () => useDragState.getState().end(),
				});
			}}
			className={ROW_CLASS}
			style={rowStyle(selected)}
		>
			<div className="flex h-[30px] min-w-0 items-center gap-2">
				<StateDot status={status} />
				<span className="shrink-0 text-[13px]" style={{ color: 'var(--fg)' }}>
					{name}
				</span>
				<span className="min-w-0 flex-1 truncate text-[11px]" title={figures.ctx ? session.title : `context ${UNREPORTED}`}>
					{`${engine} · ${live ? 'live' : 'idle'} · ${figures.ctx ?? '—'} ctx`}
				</span>
				<span className="ml-auto flex shrink-0 items-center gap-1">
					{pending > 0 && (
						<Signal tone="ask" icon={<ShieldAlert aria-hidden="true" />} title={`${pending} permission${pending === 1 ? '' : 's'} pending`}>
							{String(pending)}
						</Signal>
					)}
					{mountSignal(mount, false)}
					{showSeatButton && (
						<button
							type="button"
							tabIndex={-1}
							disabled={!canSeat}
							onClick={() => openSeatForm({ seatSession: session.id })}
							title={
								canSeat
									? 'Seat this session — give it a name that outlives its pane'
									: session.engineId
										? 'Only a running session can be seated'
										: 'A plain shell has no engine to seat'
							}
							className="h-5 rounded-sm border px-2 text-[11px] text-[var(--fg)] hover:bg-[var(--bg-sunken)] disabled:cursor-not-allowed disabled:opacity-60"
							style={{ borderColor: 'var(--border)' }}
						>
							Seat…
						</button>
					)}
				</span>
			</div>
			{selected && (
				<div className="-mt-[3px] flex min-w-0 items-center gap-2 pb-2 pl-[14px] text-[11px]">
					<span className="min-w-0 truncate">no address yet — seat it to keep its name</span>
					<button
						type="button"
						tabIndex={-1}
						aria-haspopup="menu"
						aria-label="Session actions"
						title="Session actions (Shift+F10)"
						onClick={(e) => {
							const r = e.currentTarget.getBoundingClientRect();
							onMenu(session.id, r.left, r.bottom + 4);
						}}
						className="ml-auto grid size-5 shrink-0 place-items-center rounded-sm hover:bg-[var(--bg-raised)] hover:text-[var(--fg)]"
					>
						<MoreHorizontal className="h-3.5 w-3.5" aria-hidden="true" />
					</button>
				</div>
			)}
		</div>
	);
}

// ─── Menus ──────────────────────────────────────────────────────────────────

export function seatMenuItems(
	seat: SeatView,
	isTarget: boolean,
	mount: Mount,
	live: boolean,
	run: RunAttachState | undefined
): SeatMenuItem[] {
	const vacant = seat.status === 'vacant';
	const inWindow = mount.where === 'window';
	// WP-69 (§4.4): a persistent-run seat opens / pops out a terminal attached
	// to its tmux session; a one-off run stays "headless run — nothing to show".
	const noPane = seatPaneBlocker(seat, live, run);
	const items: SeatMenuItem[] = [];
	// §5.5: only while another client holds the seat.
	if (heldByOther(seat.hold, UI_SEAT_CLIENT)) {
		items.push(
			{
				label: 'Take over',
				sub: holdText(seat.hold, formatSeatTime) ?? undefined,
				title: 'Take this seat from the client holding it (it is told on its next call)',
				run: () => void takeOverSeat(seat),
			},
			{ sep: true }
		);
	}
	items.push(
		{
			label: inWindow ? 'Open in pane — back to main window' : 'Open in pane',
			disabled: Boolean(noPane),
			title: noPane,
			run: () => openSeatInPane(seat, run),
		},
		{
			label: 'Make dispatch target',
			disabled: isTarget,
			title: isTarget ? 'Already the dispatch target' : '',
			run: () => makeTarget({ kind: 'seat', seat_id: seat.id }, seatSessionRef(seat)),
		},
		{ label: 'Open scratchpad', sub: seat.address, run: () => openSeatScratchpad(seat) },
		{
			label: 'Pop out',
			sub: inWindow ? 'in Window 2' : 'to Window 2',
			disabled: Boolean(noPane) || inWindow,
			title: inWindow ? 'Already in Window 2' : noPane,
			run: () => popOutSeat(seat, run),
		},
		{ label: 'All seats', sub: 'seat board', run: openSeatBoard },
		{ sep: true },
		{
			label: 'Rename…',
			sub: 'F2',
			run: () => {
				selectSeat(seat);
				startRename(seat.id);
			},
		},
		{ label: 'Copy address', sub: atName(seat.name), run: () => copyText(atName(seat.name), `Copied ${atName(seat.name)}`) },
		{
			label: 'Copy as iyke',
			run: () => {
				const cmd = iykeSendToSeat(seat.name, '');
				copyText(`iyke ${cmd}`, `Copied iyke ${cmd}`);
			},
		},
		{ sep: true }
	);
	if (seat.hold && seat.hold.client === UI_SEAT_CLIENT && seat.hold.expires_at > Date.now()) {
		items.push({ label: 'Release hold', sub: holdText(seat.hold, formatSeatTime) ?? undefined, run: () => void releaseSeat(seat) });
	}
	items.push(
		{
			label: 'End session',
			sub: 'the seat stays',
			disabled: vacant,
			title: vacant ? 'No session in this seat' : '',
			run: () => void endSeatSession(seat),
		},
		{
			label: 'Remove seat…',
			sub: 'Del',
			danger: true,
			run: () => useSeatUi.setState({ confirmRemove: seat.id }),
		}
	);
	return items;
}

export function sessionMenuItems(session: UnseatedSession, isTarget: boolean, mount: Mount): SeatMenuItem[] {
	const name = sessionName(session.id);
	const live = session.status === 'running';
	const inWindow = mount.where === 'window';
	const items: SeatMenuItem[] = [
		{
			label: 'Seat this session…',
			disabled: !session.engineId || !live,
			title: !session.engineId ? 'A plain shell has no engine to seat' : !live ? 'Only a running session can be seated' : '',
			run: () => openSeatForm({ seatSession: session.id }),
		},
		{ sep: true },
		{
			label: inWindow ? 'Open in pane — back to main window' : 'Open in pane',
			run: () => openSessionInPane(session.id),
		},
		{
			label: 'Make dispatch target',
			disabled: isTarget,
			title: isTarget ? 'Already the dispatch target' : '',
			run: () => makeTarget({ kind: 'session', session_id: session.id }, session.id),
		},
		{
			label: 'Pop out',
			sub: 'to Window 2',
			disabled: inWindow || !live,
			title: inWindow ? 'Already in Window 2' : !live ? 'Its terminal isn’t running' : '',
			run: () => popOutTerminal(session.id, name),
		},
		{
			label: 'Copy as iyke',
			run: () => {
				const cmd = iykeSendToTerminal(session.id, '');
				copyText(`iyke ${cmd}`, `Copied iyke ${cmd}`);
			},
		},
	];
	// The session tab's own actions (spec §3.11 #66) stay reachable for a
	// terminal parked in the Companion by a pane drop (D-09 rule 7).
	if (session.parkedIdx !== null) {
		const idx = session.parkedIdx;
		items.push(
			{ sep: true },
			{
				label: 'Move to pane',
				run: () => {
					const view = useCompanionStore.getState().tabs[idx];
					const panes = usePaneStore.getState();
					if (view && panes.placeView(panes.focusedId, { ...view, pinned: undefined }, 'append')) {
						useCompanionStore.getState().closeTab(idx);
					}
				},
			},
			{ label: 'Remove from Companion', run: () => useCompanionStore.getState().closeTab(idx) }
		);
	}
	items.push({ sep: true }, { label: 'End session', disabled: !live, run: () => void endUnseatedSession(session.id) });
	return items;
}

function SeatMenuHost({
	menu,
	roster,
	target,
	onClose,
}: {
	menu: MenuState;
	roster: SeatRoster;
	target: CompanionTarget;
	onClose: (restoreFocus: boolean) => void;
}) {
	const seat = menu.kind === 'seat' ? roster.seats.find((s) => s.id === menu.seatId) : undefined;
	const session = menu.kind === 'session' ? roster.unseated.find((u) => u.id === menu.id) : undefined;
	// WP-69: a persistent-run seat's mount is its tmux-attached terminal's
	// (§4.4); subscribing here also re-renders the menu once the run's tmux
	// session is known (`seatPaneBlocker` takes `runAttach.state`).
	const runAttach = useRunAttachedTerminal(
		seat?.session?.kind === 'run' ? { runId: seat.session.run_id, engineId: seat.engine_id } : null
	);
	const terminalId =
		seat?.session?.kind === 'terminal'
			? seat.session.terminal_id
			: (runAttach.terminalId ?? session?.id ?? null);
	const mount = useMount(terminalId);
	const live = useTerminalLive(terminalId);
	if (seat) {
		const isTarget = target.kind === 'seat' && target.seat_id === seat.id;
		return (
			<SeatMenu
				label={`Seat actions for @${seat.name}`}
				x={menu.x}
				y={menu.y}
				items={seatMenuItems(seat, isTarget, mount, live, runAttach.state)}
				onClose={onClose}
			/>
		);
	}
	if (session) {
		const isTarget = target.kind === 'session' && target.session_id === session.id;
		return (
			<SeatMenu
				label={`Session actions for ${sessionName(session.id)}`}
				x={menu.x}
				y={menu.y}
				items={sessionMenuItems(session, isTarget, mount)}
				onClose={onClose}
			/>
		);
	}
	return null;
}

// ─── The rail ───────────────────────────────────────────────────────────────

/** Where the selection goes when a selected seat is removed (D-09 `removeSeat`). */
export function nextAfterRemove(
	seat: SeatView,
	roster: Pick<SeatRoster, 'seats' | 'unseated'>
): { sel: RailSelection; scope: string | null } | null {
	if (seat.session?.kind === 'terminal' && seat.status !== 'vacant') {
		return { sel: { kind: 'session', session_id: seat.session.terminal_id }, scope: seat.session.terminal_id };
	}
	const other = roster.seats.find((s) => s.id !== seat.id);
	if (other) return { sel: { kind: 'seat', seat_id: other.id }, scope: seatSessionRef(other) };
	const u = roster.unseated[0];
	if (u) return { sel: { kind: 'session', session_id: u.id }, scope: u.id };
	return null;
}

export function SeatRail({ roster }: { roster: SeatRoster }) {
	const { seats, unseated, state, error, pendingBySession, removingNames } = roster;
	const sel = useCompanionStore((s) => s.railSelection);
	const target = useShellStore((s) => s.companion.activeTarget);
	const defaultEngine = useShellStore((s) => s.defaultEngineId);
	const draft = useCompanionStore((s) => s.draft);
	const renaming = useSeatUi((s) => s.renaming);
	const [menu, setMenu] = useState<MenuState | null>(null);
	const listRef = useRef<HTMLDivElement | null>(null);
	const returnFocusRef = useRef<string | null>(null);

	const options = useMemo(
		() => [
			...seats.map((s) => ({ key: `seat:${s.id}`, seat: s as SeatView | undefined, session: undefined as UnseatedSession | undefined })),
			...unseated.map((u) => ({ key: `session:${u.id}`, seat: undefined, session: u as UnseatedSession | undefined })),
		],
		[seats, unseated]
	);
	const selectedKey = sel ? (sel.kind === 'seat' ? `seat:${sel.seat_id}` : `session:${sel.session_id}`) : null;
	const selectedIdx = options.findIndex((o) => o.key === selectedKey);
	const roveIdx = selectedIdx >= 0 ? selectedIdx : 0;
	const takenNames = seats.map((s) => s.name);
	const empty = state === 'ready' && seats.length === 0;

	const focusKey = useCallback((key: string) => {
		const el = listRef.current?.querySelector<HTMLElement>(
			key.startsWith('seat:') ? `[data-seat="${key.slice(5)}"]` : `[data-session="${key.slice(8)}"]`
		);
		el?.focus();
	}, []);

	const pick = useCallback(
		(i: number) => {
			const o = options[i];
			if (!o) return;
			if (o.seat) selectSeat(o.seat);
			else if (o.session) selectSession(o.session.id);
			// Focus follows once the row re-renders as selected.
			setTimeout(() => focusKey(o.key), 0);
		},
		[options, focusKey]
	);

	const openMenuFor = useCallback((m: MenuState) => {
		returnFocusRef.current = m.kind === 'seat' ? `seat:${m.seatId}` : `session:${m.id}`;
		setMenu(m);
	}, []);

	const closeMenu = useCallback(
		(restore: boolean) => {
			setMenu(null);
			const key = returnFocusRef.current;
			returnFocusRef.current = null;
			// Checked when the timer fires: *Rename…* opens its field after the
			// menu closes, and the field must keep the focus.
			if (restore && key) {
				setTimeout(() => {
					if (!useSeatUi.getState().renaming) focusKey(key);
				}, 0);
			}
		},
		[focusKey]
	);

	function onKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
		const row = (e.target as HTMLElement).closest<HTMLElement>('[role="option"]');
		const tag = (e.target as HTMLElement).tagName;
		// A row's own buttons and the rename field handle their own keys.
		if (!row || tag === 'INPUT' || tag === 'BUTTON') return;
		const i = options.findIndex((o) =>
			o.seat ? row.dataset.seat === o.seat.id : row.dataset.session === o.session?.id
		);
		const o = options[i];
		if (!o) return;
		if (e.altKey) return; // ⌥↑/⌥↓ cycle the target (the Companion's handler)
		if (e.key === 'ArrowDown') {
			e.preventDefault();
			pick(Math.min(options.length - 1, i + 1));
		} else if (e.key === 'ArrowUp') {
			e.preventDefault();
			pick(Math.max(0, i - 1));
		} else if (e.key === 'Home') {
			e.preventDefault();
			pick(0);
		} else if (e.key === 'End') {
			e.preventDefault();
			pick(options.length - 1);
		} else if (e.key === ' ') {
			e.preventDefault();
			pick(i);
		} else if (e.key === 'Enter') {
			e.preventDefault();
			const tid = o.seat
				? o.seat.session?.kind === 'terminal' && o.seat.status !== 'vacant'
					? o.seat.session.terminal_id
					: null
				: (o.session?.id ?? null);
			if (tid) openSessionInPane(tid);
		} else if (e.key === 'F2' && o.seat) {
			e.preventDefault();
			selectSeat(o.seat);
			startRename(o.seat.id);
		} else if (e.key === 'Delete' && o.seat) {
			e.preventDefault();
			useSeatUi.setState({ confirmRemove: o.seat.id });
		} else if (e.key === 'ContextMenu' || (e.key === 'F10' && e.shiftKey)) {
			e.preventDefault();
			const r = row.getBoundingClientRect();
			if (o.seat) openMenuFor({ kind: 'seat', seatId: o.seat.id, x: r.left + 24, y: r.top + 28 });
			else if (o.session) openMenuFor({ kind: 'session', id: o.session.id, x: r.left + 24, y: r.top + 28 });
		}
	}

	// The empty state's *Seat this session…*: the selected session, else the first seatable one.
	const seatable = unseated.filter((u) => u.engineId && u.status === 'running');
	const emptySeatTarget =
		(sel?.kind === 'session' ? seatable.find((u) => u.id === sel.session_id) : undefined) ?? seatable[0];

	return (
		<div
			data-state={empty ? 'seats-empty' : 'seats-roster'}
			className="flex max-h-[52%] shrink-0 flex-col border-b"
			style={{ background: 'var(--bg-base)', borderColor: 'var(--border)' }}
		>
			<div id="seats-head" className="flex h-[26px] shrink-0 items-center gap-2 pl-3 pr-2">
				<span className="text-[11px] font-semibold uppercase tracking-[.1em]" style={{ color: 'var(--fg-muted)' }}>
					Seats
				</span>
				{seats.length > 0 && (
					<span className="font-mono text-[11px]" style={{ color: 'var(--fg-muted)' }}>
						{seats.length}
					</span>
				)}
				<span className="ml-auto flex items-center gap-1">
					<button
						type="button"
						onClick={openSeatBoard}
						aria-label="All seats"
						title="All seats — open the seat board"
						className="grid size-6 place-items-center rounded-sm text-[var(--fg-muted)] hover:bg-[var(--bg-raised)] hover:text-[var(--fg)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
					>
						<LayoutGrid className="h-3.5 w-3.5" aria-hidden="true" />
					</button>
					{!empty && (
						<button
							type="button"
							onClick={() => openSeatForm()}
							aria-label="New seat"
							title="New seat"
							data-new-seat=""
							className="grid size-6 place-items-center rounded-sm text-[var(--fg-muted)] hover:bg-[var(--bg-raised)] hover:text-[var(--fg)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
						>
							<Plus className="h-3.5 w-3.5" aria-hidden="true" />
						</button>
					)}
				</span>
			</div>

			{empty && (
				<div className="px-3 pb-3 pt-2">
					<p className="max-w-[40ch] text-[13px] leading-[1.5]" style={{ color: 'var(--fg)' }}>
						A seat keeps an agent’s name when its pane moves or its session ends.
					</p>
					<div className="mt-3 flex gap-2">
						<button
							type="button"
							data-new-seat=""
							onClick={() => openSeatForm()}
							className="h-7 rounded-md bg-[var(--primary)] px-3 text-xs text-[var(--primary-fg)] hover:opacity-90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
						>
							New seat
						</button>
						<button
							type="button"
							disabled={!emptySeatTarget}
							title={emptySeatTarget ? `Seat ${sessionName(emptySeatTarget.id)} — the selected session` : 'No open sessions to seat'}
							onClick={() => emptySeatTarget && openSeatForm({ seatSession: emptySeatTarget.id })}
							className="h-7 rounded-md border px-3 text-xs text-[var(--fg)] hover:bg-[var(--bg-raised)] disabled:cursor-not-allowed disabled:opacity-60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
							style={{ borderColor: 'var(--border)' }}
						>
							Seat this session…
						</button>
					</div>
				</div>
			)}

			{state === 'error' && (
				<p role="status" className="px-3 pb-2 text-[11px]" style={{ color: 'var(--fg-muted)' }}>
					Seats couldn’t be read{error ? ` — ${error}` : ''}.
				</p>
			)}

			<div
				ref={listRef}
				role="listbox"
				aria-label="Seats and unseated sessions"
				onKeyDown={onKeyDown}
				className="min-h-0 flex-[0_1_auto] overflow-y-auto"
			>
				{/* biome-ignore lint/a11y/useSemanticElements: an ARIA listbox group, not a form fieldset */}
				<div role="group" aria-labelledby="seats-head">
					{seats.map((seat, i) => {
						const ref = seat.session?.kind === 'terminal' ? seat.session.terminal_id : null;
						return (
							<SeatRow
								key={seat.id}
								seat={seat}
								selected={selectedKey === `seat:${seat.id}`}
								focusable={i === roveIdx}
								renaming={renaming === seat.id}
								takenNames={takenNames}
								removingNames={removingNames}
								pending={ref && seat.status !== 'vacant' ? (pendingBySession[ref] ?? 0) : 0}
								onMenu={(s, x, y) => openMenuFor({ kind: 'seat', seatId: s.id, x, y })}
							/>
						);
					})}
				</div>
				{(seats.length > 0 || unseated.length > 0) && (
					<>
						<div
							id="unseated-head"
							className="flex h-[26px] items-center gap-2 border-t pl-3 pr-2"
							style={{ borderColor: 'var(--border-soft)' }}
						>
							<span className="text-[11px] font-semibold uppercase tracking-[.1em]" style={{ color: 'var(--fg-muted)' }}>
								Unseated
							</span>
							{unseated.length > 0 && (
								<span className="font-mono text-[11px]" style={{ color: 'var(--fg-muted)' }}>
									{unseated.length}
								</span>
							)}
						</div>
						{/* biome-ignore lint/a11y/useSemanticElements: an ARIA listbox group, not a form fieldset */}
						<div role="group" aria-labelledby="unseated-head">
							{unseated.map((u, j) => (
								<UnseatedRow
									key={u.id}
									session={u}
									selected={selectedKey === `session:${u.id}`}
									focusable={seats.length + j === roveIdx}
									pending={pendingBySession[u.id] ?? 0}
									showSeatButton={!empty}
									onMenu={(id, x, y) => openMenuFor({ kind: 'session', id, x, y })}
								/>
							))}
							{unseated.length === 0 && (
								<div className="pb-2 pl-[26px] pr-3 text-[11px]" style={{ color: 'var(--fg-muted)' }}>
									No unseated sessions.
								</div>
							)}
						</div>
					</>
				)}
			</div>

			<RailIykeLine cmd={iykeForTarget(target, seats, draft, defaultEngine)} />

			{menu && <SeatMenuHost menu={menu} roster={roster} target={target} onClose={closeMenu} />}
		</div>
	);
}
