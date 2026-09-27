// WP-68 — the `/chi` seat board (D-09 `seats-board.html`, the SECONDARY seat
// surface of the hybrid lock; DEC-66, DEC-67, G-SEATS §8).
//
// "When you want the whole roster at once — every seat and every unseated
// session with its session, state, context, cost, what it waits on, its
// scratchpad and where it is mounted — /chi opens it as a board in the
// focused pane; the rail stays the place you select, dispatch and act."
//
// The board DRAWS the rail's model and CALLS the rail's actions; it forks
// neither. The roster is `useSeatRoster()` (the rail's read), the menus are
// the rail's own `seatMenuItems` / `sessionMenuItems` in the rail's
// `SeatMenu`, the vacant seat's detail embeds the rail's `SeatVacantPanel`,
// and every write is a `seat-actions.ts` function. So the rail and the board
// cannot disagree.
//
// Board-only rules (D-09):
// - browsing rows never retargets dispatch — the selection is board-local
//   (`board-store.ts`); *Make dispatch target* does, through `makeTarget`;
// - actions that open something "in the focused pane" are pointed at the
//   pane beside the board first (`focusBesideBoard`), so the board stays put;
// - figures an engine hasn't reported read "—" with `UNREPORTED`.
//
//   click        select (board-local)
//   dbl / ↵      open its terminal in a pane (a vacant seat: resume it)
//   right / ⋯    the rail's seat menu (Esc / outside click close it)
//   ↑ ↓ Home End rove + select (one tab stop, roving tabindex)
//   Shift+F10 / ContextMenu   the menu, from the keyboard
//
// State map (G-55 `data-state` on the root, `boardState()`): `board-roster`,
// `board-empty`, `board-create`, `board-vacant`, `board-popout`, plus
// `board-loading` / `board-error`. D-09 `dispatch` is the rail's picker
// (`seats-dispatch`) over a `board-roster` board; `rest` is no board.
//
// ADR-021: rows and the detail carry state and addresses only — never model
// output.

import {
	AppWindow,
	AtSign,
	Clock,
	Copy,
	FileEdit,
	Hourglass,
	Inbox,
	Lock,
	Minus,
	MoreHorizontal,
	PanelRight,
	Plus,
	Send,
	ShieldAlert,
	TerminalSquare,
} from 'lucide-react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useShellStore } from '@/lib/shell/shell-store';
import type { SeatView } from '@/lib/tauri-cmd';
import { useDetachedSurfaces } from '@/lib/window/detached-surfaces';
import { useCompanionStore } from '@/shell/companion/companion-store';
import { engineRunsInTerminal } from '@/shell/companion/resolve-target';
import {
	copyText,
	makeTarget,
	type Mount,
	mountOfTerminal,
	openSeatForm,
	openSeatScratchpad,
	openSessionInPane,
	resumeSeat,
	selectSeat,
	useSeatUi,
} from '@/shell/companion/seat-actions';
import { SeatMenu, type SeatMenuItem } from '@/shell/companion/seat-menu';
import {
	atName,
	engineResumeFlag,
	engineShort,
	holdText,
	seatSessionRef,
	UNREPORTED,
} from '@/shell/companion/seat-model';
import { useRunAttachedTerminal } from '@/terminal/attach-run';
import { formatSeatTime } from '@/shell/companion/seat-notice';
import { seatMenuItems, sessionMenuItems } from '@/shell/companion/seat-rail';
import { SeatRemoveDialog } from '@/shell/companion/seat-remove-dialog';
import { type UnseatedSession, useSeatRoster } from '@/shell/companion/seat-roster';
import { sessionName, useSessionFigures, useSessionFiguresStore } from '@/shell/companion/seat-sessions';
import { SeatVacantPanel } from '@/shell/companion/seat-vacant-panel';
import { usePaneScope } from '@/shell/panes/pane-scope';
import { useTerminalStore } from '@/terminal/session-store';
import {
	type BoardRow,
	type BoardSelection,
	boardIyke,
	boardRows,
	boardState,
	countLine,
	type MountText,
	mountSignature,
	mountText,
	movedToWindow,
	moveSelection,
	padLatest,
	relativeTime,
	rowKey,
	selectionOf,
	stateWord,
	validSelection,
} from './board-model';
import { consumeBoardFocus, focusBesideBoard, selectBoardRow, useBoardUi } from './board-store';
import './chi-board.css';

/** How long the "moved" highlight runs: D-09's `hitpanel` 1.6 s, twice. */
export const MOVED_MS = 3_200;

function cssEscape(value: string): string {
	return typeof CSS !== 'undefined' && typeof CSS.escape === 'function'
		? CSS.escape(value)
		: value.replace(/["\\]/g, '\\$&');
}

// ─── Mounts and the "moved" highlight (G-93, G-96) ─────────────────────────

/** Every row's mount (a readout, never the address), computed once. */
function useBoardMounts(seats: readonly SeatView[], unseated: readonly UnseatedSession[]): Map<string, Mount> {
	const root = usePaneStore((s) => s.root);
	const detached = useDetachedSurfaces((s) => s.surfaceToWindow);
	const tabs = useTerminalStore((s) => s.tabs);
	return useMemo(() => {
		const ptyOf = (id: string) => tabs.find((t) => t.id === id)?.ptyId ?? null;
		const out = new Map<string, Mount>();
		for (const seat of seats) {
			const tid = seat.session?.kind === 'terminal' && seat.status !== 'vacant' ? seat.session.terminal_id : null;
			out.set(`seat:${seat.id}`, tid ? mountOfTerminal(tid, root, detached, ptyOf(tid)) : { where: 'none' });
		}
		for (const u of unseated) out.set(`session:${u.id}`, mountOfTerminal(u.id, root, detached, ptyOf(u.id)));
		return out;
	}, [seats, unseated, root, detached, tabs]);
}

/** Rows whose mount just became a second window: their Window 2 chip
 *  pulses (D-09 `.sgl.moved`), and the root reads `board-popout`. */
function useMovedRows(mounts: Map<string, Mount>): ReadonlySet<string> {
	const prev = useRef<Map<string, string> | null>(null);
	const [moved, setMoved] = useState<ReadonlySet<string>>(() => new Set());
	useEffect(() => {
		const next = new Map<string, string>();
		const hits: string[] = [];
		for (const [key, mount] of mounts) {
			const sig = mountSignature(mount);
			next.set(key, sig);
			if (prev.current && movedToWindow(prev.current.get(key), sig)) hits.push(key);
		}
		prev.current = next;
		if (hits.length) setMoved((cur) => new Set([...cur, ...hits]));
	}, [mounts]);
	useEffect(() => {
		if (moved.size === 0) return;
		const t = setTimeout(() => setMoved(new Set()), MOVED_MS);
		return () => clearTimeout(t);
	}, [moved]);
	return moved;
}

// ─── Cells ──────────────────────────────────────────────────────────────────

function TargetIcon() {
	return (
		<span title="Dispatch target" className="inline-flex">
			<Send className="tgtico" role="img" aria-label="dispatch target" />
		</span>
	);
}

function Num({ value, title }: { value: string | null; title?: string }) {
	if (!value) {
		return (
			<span className="num dim" title={UNREPORTED}>
				—
			</span>
		);
	}
	return (
		<span className="num" title={title}>
			{value}
		</span>
	);
}

function MountCell({ text, moved }: { text: MountText; moved: boolean }) {
	if (text.where === 'vacant') return <span className="t">—</span>;
	if (text.where === 'window') {
		return (
			<span
				className="w2chip"
				data-moved={moved ? 'true' : undefined}
				title="Popped out to a second window · the address is unchanged"
			>
				<AppWindow aria-hidden="true" />
				<span className="t long">{text.long}</span>
				<span className="t short">{text.short}</span>
			</span>
		);
	}
	const Icon = text.where === 'main' ? PanelRight : text.where === 'headless' ? Clock : Minus;
	return (
		<>
			<Icon aria-hidden="true" />
			<span className="t long">{text.long}</span>
			<span className="t short" title={text.long}>
				{text.short}
			</span>
		</>
	);
}

function PendingCell({
	pending,
	seat,
}: {
	pending: number;
	seat: SeatView | null;
}) {
	const held = seat ? holdText(seat.hold, formatSeatTime) : null;
	return (
		<>
			{pending > 0 && (
				<span className="pp" title={`${pending} permission${pending === 1 ? '' : 's'} pending`}>
					<ShieldAlert aria-hidden="true" />
					{pending}
				</span>
			)}
			{pending === 0 && seat && seat.status !== 'vacant' && seat.inbox_count > 0 && (
				<span className="pp inbox" title={`Inbox: ${seat.inbox_count}`}>
					<Inbox aria-hidden="true" />
					{seat.inbox_count}
				</span>
			)}
			{seat?.queued && (
				<span className="pp plain" title="Queued text — it sends when the run finishes">
					<Hourglass aria-hidden="true" />
				</span>
			)}
			{held && (
				<span className="pp" title={held} aria-label={held}>
					<Lock aria-hidden="true" />
				</span>
			)}
		</>
	);
}

// ─── Rows ───────────────────────────────────────────────────────────────────

interface RowCommon {
	selected: boolean;
	focusable: boolean;
	mount: Mount;
	moved: boolean;
	pending: number;
	isTarget: boolean;
	onSelect: () => void;
	onPrimary: () => void;
	onMenu: (x: number, y: number) => void;
}

function SeatBoardRow({ seat, ...p }: RowCommon & { seat: SeatView }) {
	const ref = seatSessionRef(seat);
	const figures = useSessionFigures(ref);
	const vacant = seat.status === 'vacant';
	const isRun = seat.session?.kind === 'run';
	const mount = mountText(p.mount, { isRun, vacant });
	const flag = engineResumeFlag(seat.engine_resume);
	const pad = padLatest(seat);
	const name = ref ? sessionName(ref) : null;
	let sess: React.ReactNode;
	if (!vacant) {
		sess = isRun ? (
			<>
				<span className="dim">run</span> {name}
			</>
		) : (
			name
		);
	} else {
		const text = name ? `${name} ended` : 'none — cleared';
		sess = (
			<span className="dim" title={name ? `${text} · last active ${relativeTime(seat.last_active_at)}` : text}>
				{text}
			</span>
		);
	}
	return (
		<div
			role="row"
			tabIndex={p.focusable ? 0 : -1}
			aria-selected={p.selected}
			data-row-key={`seat:${seat.id}`}
			data-seat={seat.id}
			data-status={seat.status}
			className={`srow2 sgrid${vacant ? ' vac' : ''}`}
			onClick={(e) => {
				if ((e.target as HTMLElement).closest('button')) return;
				p.onSelect();
			}}
			onDoubleClick={(e) => {
				if ((e.target as HTMLElement).closest('button')) return;
				p.onPrimary();
			}}
			onContextMenu={(e) => {
				e.preventDefault();
				p.onSelect();
				p.onMenu(e.clientX, e.clientY);
			}}
		>
			<span className="c-name" role="gridcell">
				<span className="nm">
					<span className="at">@</span>
					{seat.name}
				</span>
				{p.isTarget && <TargetIcon />}
			</span>
			<span className="c-eng" role="gridcell" title={flag ? `${seat.engine_id} · ${flag}` : seat.engine_id}>
				<span className="t">{flag ? `${seat.engine_id} · ${flag}` : seat.engine_id}</span>
			</span>
			<span className="c-sess" role="gridcell">
				{sess}
			</span>
			<span className="c-state" role="gridcell">
				<span className={`state s-${seat.status}`}>{stateWord(seat)}</span>
			</span>
			<span className="c-ctx" role="gridcell">
				{vacant ? (
					<Num value={figures.ctx} title="context at end" />
				) : (
					<Num value={figures.ctx} title={figures.ctx ? 'context, tokens' : undefined} />
				)}
			</span>
			<span className="c-cost" role="gridcell">
				{vacant ? (
					<span className="num dim" title="No session in this seat">
						—
					</span>
				) : (
					<Num value={figures.amt} />
				)}
			</span>
			<span className="c-pend" role="gridcell">
				<PendingCell pending={vacant ? 0 : p.pending} seat={seat} />
			</span>
			<span className="c-pad" role="gridcell" title={`${seat.address} · ${pad.count}`}>
				<FileEdit aria-hidden="true" />
				<span className="sc2">{seat.address}</span>
				{pad.latest ? (
					<>
						<span className="lt" title="latest entry">
							{pad.latest}
						</span>
						<span className="n" title={pad.count}>
							· {seat.pad.count}
						</span>
					</>
				) : (
					<span className="lt dim">{pad.count}</span>
				)}
			</span>
			<span className="c-mnt" role="gridcell">
				<MountCell text={mount} moved={p.moved} />
			</span>
			<span className="c-act" role="gridcell">
				<button
					type="button"
					tabIndex={-1}
					className="iconbtn sm"
					aria-haspopup="menu"
					aria-label={`Seat actions for @${seat.name}`}
					title="Seat actions (Shift+F10)"
					onClick={(e) => {
						e.stopPropagation();
						p.onSelect();
						const r = e.currentTarget.getBoundingClientRect();
						p.onMenu(r.left, r.bottom + 4);
					}}
				>
					<MoreHorizontal aria-hidden="true" />
				</button>
			</span>
		</div>
	);
}

function canSeatSession(u: UnseatedSession): boolean {
	return u.engineId !== null && u.status === 'running';
}

function seatSessionTitle(u: UnseatedSession): string {
	if (canSeatSession(u)) return 'Seat this session — give it a name that outlives its pane';
	return u.engineId ? 'Only a running session can be seated' : 'A plain shell has no engine to seat';
}

function SessionBoardRow({ session, ...p }: RowCommon & { session: UnseatedSession }) {
	const figures = useSessionFigures(session.id);
	const live = session.status === 'running' || session.status === 'spawning';
	const mount = mountText(p.mount, { isRun: false, vacant: false });
	const name = sessionName(session.id);
	return (
		<div
			role="row"
			tabIndex={p.focusable ? 0 : -1}
			aria-selected={p.selected}
			data-row-key={`session:${session.id}`}
			data-session={session.id}
			className="srow2 sgrid"
			onClick={(e) => {
				if ((e.target as HTMLElement).closest('button')) return;
				p.onSelect();
			}}
			onDoubleClick={(e) => {
				if ((e.target as HTMLElement).closest('button')) return;
				p.onPrimary();
			}}
			onContextMenu={(e) => {
				e.preventDefault();
				p.onSelect();
				p.onMenu(e.clientX, e.clientY);
			}}
		>
			<span className="c-name" role="gridcell">
				<span className="none">no seat</span>
				{p.isTarget && <TargetIcon />}
			</span>
			<span className="c-eng" role="gridcell">
				<span className="t">{session.engineId ?? 'shell'}</span>
			</span>
			<span className="c-sess" role="gridcell">
				{name}
			</span>
			<span className="c-state" role="gridcell">
				<span className={`state s-${live ? 'live' : 'idle'}`}>{live ? 'live' : 'idle'}</span>
			</span>
			<span className="c-ctx" role="gridcell">
				<Num value={figures.ctx} title={figures.ctx ? 'context, tokens' : undefined} />
			</span>
			<span className="c-cost" role="gridcell">
				<Num value={figures.amt} />
			</span>
			<span className="c-pend" role="gridcell">
				<PendingCell pending={p.pending} seat={null} />
			</span>
			<span className="c-pad" role="gridcell">
				<button
					type="button"
					className="btn"
					data-board-seat-this=""
					disabled={!canSeatSession(session)}
					title={seatSessionTitle(session)}
					onClick={(e) => {
						e.stopPropagation();
						openSeatForm({ seatSession: session.id });
					}}
				>
					<AtSign aria-hidden="true" />
					<span>Seat this session…</span>
				</button>
			</span>
			<span className="c-mnt" role="gridcell">
				<MountCell text={mount} moved={p.moved} />
			</span>
			<span className="c-act" role="gridcell">
				<button
					type="button"
					tabIndex={-1}
					className="iconbtn sm"
					aria-haspopup="menu"
					aria-label={`Session actions for ${name}`}
					title="Session actions (Shift+F10)"
					onClick={(e) => {
						e.stopPropagation();
						p.onSelect();
						const r = e.currentTarget.getBoundingClientRect();
						p.onMenu(r.left, r.bottom + 4);
					}}
				>
					<MoreHorizontal aria-hidden="true" />
				</button>
			</span>
		</div>
	);
}

// ─── The detail column ──────────────────────────────────────────────────────

function DRow({
	k,
	children,
	cls,
	title,
	rt,
}: {
	k: string;
	children: React.ReactNode;
	cls?: string;
	title?: string;
	rt?: React.ReactNode;
}) {
	return (
		<div className="drow">
			<span className="k2">{k}</span>
			<span className={`val${cls ? ` ${cls}` : ''}`} title={title}>
				{children}
			</span>
			{rt && <span className="rt">{rt}</span>}
		</div>
	);
}

function CopyAddress({ seat }: { seat: SeatView }) {
	return (
		<button
			type="button"
			className="iconbtn sm"
			title="Copy address"
			aria-label={`Copy address @${seat.name}`}
			onClick={() => copyText(atName(seat.name), `Copied ${atName(seat.name)}`)}
		>
			<Copy aria-hidden="true" />
		</button>
	);
}

/** *Review* on a pending permission: the Companion's permission panel is
 *  scoped to the rail's selection (§9.1), so the seat is selected there —
 *  an explicit act, like *Make dispatch target* — then its card focused. */
function reviewPermission(seat: SeatView): void {
	useCompanionStore.getState().setState('expanded');
	selectSeat(seat);
	const ref = seatSessionRef(seat);
	setTimeout(() => {
		const sel = ref
			? `[data-permission-card][data-permission-session="${cssEscape(ref)}"]`
			: '[data-permission-card][data-status="pending"]';
		document.querySelector<HTMLElement>(sel)?.focus();
	}, 0);
}

function SeatDetail({
	seat,
	mount,
	isTarget,
	pending,
	projectLabel,
	boardPaneId,
	onMenu,
}: {
	seat: SeatView;
	mount: Mount;
	isTarget: boolean;
	pending: number;
	projectLabel: string;
	boardPaneId: string | null;
	onMenu: (x: number, y: number) => void;
}) {
	const ref = seatSessionRef(seat);
	const figures = useSessionFigures(ref);
	const exactCtx = useSessionFiguresStore((s) => (ref ? s.snaps[ref]?.context_window?.total_input_tokens : undefined));
	const vacant = seat.status === 'vacant';
	const isRun = seat.session?.kind === 'run';
	const terminalId = seat.session?.kind === 'terminal' ? seat.session.terminal_id : null;
	const live = useTerminalStore((s) =>
		terminalId ? s.tabs.some((t) => t.id === terminalId && t.status === 'running') : false
	);
	const name = ref ? sessionName(ref) : null;
	const held = holdText(seat.hold, formatSeatTime);
	const flag = engineResumeFlag(seat.engine_resume);
	const mt = mountText(mount, { isRun, vacant });
	const openLabel = mount.where === 'window' ? 'Open in pane — back to main window' : mount.where === 'main' ? 'Go to pane' : 'Open in pane';
	const openBlocked = isRun ? 'Headless run — nothing to show in a pane' : !live ? 'Its terminal isn’t running' : null;

	const more = (
		<button
			type="button"
			className="iconbtn"
			aria-haspopup="menu"
			aria-label="More seat actions"
			title="Seat menu"
			data-board-detail-more=""
			onClick={(e) => {
				const r = e.currentTarget.getBoundingClientRect();
				onMenu(r.left, r.bottom + 4);
			}}
		>
			<MoreHorizontal aria-hidden="true" />
		</button>
	);

	return (
		<>
			<div className="dhead">
				<div className="dtitle">
					<h2 className="mono">@{seat.name}</h2>
					<span className={`state s-${seat.status}`}>{stateWord(seat)}</span>
					{isTarget && <span className="pchip tgt">dispatch target</span>}
				</div>
				<div className="dsub">
					<span>
						<b>{seat.engine_id}</b>
						{name ? ` · ${vacant ? `last ${name}` : isRun ? `run · ${name}` : name}` : ''}
					</span>
					<span>{projectLabel}</span>
				</div>
				<div className="dacts">
					{!vacant && !isTarget && (
						<button
							type="button"
							className="btn primary"
							data-board-make-target=""
							onClick={() => makeTarget({ kind: 'seat', seat_id: seat.id }, ref)}
						>
							<Send aria-hidden="true" />
							<span>Make dispatch target</span>
						</button>
					)}
					{!vacant && (
						<button
							type="button"
							className="iconbtn"
							data-board-open=""
							disabled={openBlocked !== null || !terminalId}
							title={openBlocked ?? openLabel}
							aria-label={openLabel}
							onClick={() => {
								if (!terminalId) return;
								focusBesideBoard(boardPaneId);
								openSessionInPane(terminalId);
							}}
						>
							{mount.where === 'window' ? <AppWindow aria-hidden="true" /> : <TerminalSquare aria-hidden="true" />}
						</button>
					)}
					{more}
				</div>
			</div>
			<div className="dbody">
				{vacant && (
					// The rail's own vacant panel: Resume / Fill / Clear, with its
					// disabled reasons (§6.2, E-1). Its scratchpad link opens in the
					// focused pane, so point that at the pane beside the board.
					<div
						className="dvacant"
						onClickCapture={(e) => {
							const b = (e.target as HTMLElement).closest('button');
							if (b?.title.startsWith('Open scratchpad')) focusBesideBoard(boardPaneId);
						}}
					>
						<SeatVacantPanel seat={seat} />
					</div>
				)}
				<DRow k="Address" cls="mono" rt={<CopyAddress seat={seat} />}>
					@{seat.name}
				</DRow>
				{!vacant && (
					<>
						<DRow k="Session">
							{isRun ? `run · ${name ?? 'a run'}` : `${name ?? 'session'} · ${seat.engine_id}`}
						</DRow>
						<DRow k="Mounted" title={mt.long}>
							{mt.long}
						</DRow>
						<DRow k="Context" cls="mono" title={exactCtx || figures.ctx ? undefined : UNREPORTED}>
							{exactCtx ? `${exactCtx.toLocaleString('en-US')} tokens` : figures.ctx ? `${figures.ctx} tokens` : '—'}
						</DRow>
						<DRow k="Cost" cls="mono" title={figures.amt ? undefined : UNREPORTED}>
							{figures.amt ?? '—'}
						</DRow>
						{pending > 0 ? (
							<DRow
								k="Pending"
								rt={
									<button type="button" className="btn" data-board-review="" onClick={() => reviewPermission(seat)}>
										Review
									</button>
								}
							>
								{`${pending} permission${pending === 1 ? '' : 's'}`}
							</DRow>
						) : seat.inbox_count > 0 ? (
							<DRow k="Inbox">{String(seat.inbox_count)}</DRow>
						) : null}
						{seat.queued && (
							<DRow k="Queued" title={`since ${formatSeatTime(seat.queued.since)}`}>
								a text — it sends when the run finishes
							</DRow>
						)}
					</>
				)}
				{held && (
					<DRow k="Hold" cls="warn" title={held}>
						{held}
					</DRow>
				)}
				{!vacant && (
					<>
						<DRow
							k="Scratchpad"
							cls="mono"
							title={seat.address}
							rt={
								<button
									type="button"
									className="btn"
									data-board-open-pad=""
									onClick={() => {
										focusBesideBoard(boardPaneId);
										openSeatScratchpad(seat);
									}}
								>
									Open
								</button>
							}
						>
							{`${seat.address} · ${seat.pad.count}`}
						</DRow>
						{seat.pad.latest && <DRow k="Latest">{`“${seat.pad.latest.name}”`}</DRow>}
						{flag && <DRow k="Engine">{flag}</DRow>}
					</>
				)}
				<p className="promise" data-promise="">
					Split, pop-out or a session ending never change <b>@{seat.name}</b>; only Rename does.
				</p>
			</div>
		</>
	);
}

function SessionDetail({
	session,
	mount,
	isTarget,
	projectLabel,
}: {
	session: UnseatedSession;
	mount: Mount;
	isTarget: boolean;
	projectLabel: string;
}) {
	const figures = useSessionFigures(session.id);
	const live = session.status === 'running' || session.status === 'spawning';
	const mt = mountText(mount, { isRun: false, vacant: false });
	const name = sessionName(session.id);
	return (
		<>
			<div className="dhead">
				<div className="dtitle">
					<h2>{name}</h2>
					<span className={`state s-${live ? 'live' : 'idle'}`}>{live ? 'live' : 'idle'}</span>
					{isTarget && <span className="pchip tgt">dispatch target</span>}
				</div>
				<div className="dsub">
					<span>
						<b>{session.engineId ? engineShort(session.engineId) : 'shell'}</b> · no seat
					</span>
					<span>{projectLabel}</span>
				</div>
				<div className="dacts">
					<button
						type="button"
						className="btn primary"
						data-board-detail-seat-this=""
						disabled={!canSeatSession(session)}
						title={seatSessionTitle(session)}
						onClick={() => openSeatForm({ seatSession: session.id })}
					>
						<AtSign aria-hidden="true" />
						<span>Seat this session…</span>
					</button>
				</div>
			</div>
			<div className="dbody">
				<DRow k="Address">its terminal id only</DRow>
				<DRow k="Mounted" title={mt.long}>
					{mt.long}
				</DRow>
				<DRow k="Context" cls="mono" title={figures.ctx ? undefined : UNREPORTED}>
					{figures.ctx ?? '—'}
				</DRow>
				<p className="promise" data-promise="">
					No seat: callers address it by terminal id. Seat it to give it a name that outlives its pane.
				</p>
			</div>
		</>
	);
}

// ─── The board ──────────────────────────────────────────────────────────────

type MenuState = { key: string; x: number; y: number };

export function SeatBoard() {
	const roster = useSeatRoster();
	const { seats, unseated, state, error, pendingBySession, projectId } = roster;
	const boardPaneId = usePaneScope();
	const projectLabel = useShellStore(
		(s) => s.projects.find((p) => p.id === projectId)?.display_name ?? projectId
	);
	const target = useShellStore((s) => s.companion.activeTarget);
	const railSel = useCompanionStore((s) => s.railSelection);
	const companionState = useCompanionStore((s) => s.state);
	const form = useSeatUi((s) => s.form);
	const stored = useBoardUi((s) => s.selection);
	const focusRequest = useBoardUi((s) => s.focusRequest);
	const [menu, setMenu] = useState<MenuState | null>(null);
	const rootRef = useRef<HTMLDivElement | null>(null);
	const returnFocusRef = useRef<string | null>(null);

	const rows = useMemo(() => boardRows(seats, unseated), [seats, unseated]);
	const mounts = useBoardMounts(seats, unseated);
	const moved = useMovedRows(mounts);

	// Board-local selection; until one is made here, the board follows the
	// rail's (the "lead selected" of D-09's roster).
	const railAsBoard: BoardSelection | null = railSel
		? railSel.kind === 'seat'
			? { kind: 'seat', id: railSel.seat_id }
			: { kind: 'session', id: railSel.session_id }
		: null;
	const sel = validSelection(stored, seats, unseated) ?? validSelection(railAsBoard, seats, unseated);
	const selKey = sel ? rowKey(sel) : null;
	const selRow = selKey ? (rows.find((r) => rowKey(r) === selKey) ?? null) : null;
	const tabStopKey = selKey ?? (rows[0] ? rowKey(rows[0]) : null);

	const focusRow = useCallback((key: string) => {
		rootRef.current?.querySelector<HTMLElement>(`[data-row-key="${cssEscape(key)}"]`)?.focus();
	}, []);

	const select = useCallback(
		(row: BoardRow, focus?: boolean) => {
			selectBoardRow(selectionOf(row));
			if (focus) setTimeout(() => focusRow(rowKey(row)), 0);
		},
		[focusRow]
	);

	// "Create lands on the board": a seat created through the Companion form
	// (which selects it in the rail) becomes the board's selection too.
	const railAtFormOpen = useRef<string | null>(null);
	const formOpen = form !== null;
	const railKey = railAsBoard ? rowKey(railAsBoard) : null;
	// biome-ignore lint/correctness/useExhaustiveDependencies: railAsBoard is derived from railKey
	useEffect(() => {
		if (formOpen) {
			railAtFormOpen.current = railKey;
			return;
		}
		if (railAtFormOpen.current !== null && railKey && railKey !== railAtFormOpen.current && railAsBoard) {
			selectBoardRow(railAsBoard);
		}
		railAtFormOpen.current = null;
	}, [formOpen, railKey]);

	// Keyboard focus lands on the selected row when an entry point asks for it
	// (`requestBoardFocus`, which every entry point sends through
	// `openSeatBoard()` — the rail's ⊞ and *All seats* included). Once the
	// roster is ready the request is always consumed, so a request made while
	// the board had no rows can't steal focus on a later remount.
	const ready = state === 'ready';
	// biome-ignore lint/correctness/useExhaustiveDependencies: only a request matters; the tab stop is read at that moment
	useEffect(() => {
		if (!ready) return;
		const want = consumeBoardFocus();
		if (want && tabStopKey) setTimeout(() => focusRow(tabStopKey), 0);
	}, [ready, focusRequest]);

	const openMenu = useCallback((key: string, x: number, y: number) => {
		returnFocusRef.current = key;
		setMenu({ key, x, y });
	}, []);

	const closeMenu = useCallback(
		(restore: boolean) => {
			setMenu(null);
			const key = returnFocusRef.current;
			returnFocusRef.current = null;
			if (restore && key) {
				// Not when the item handed focus on: the rail's rename field, the
				// Remove confirm, or the New-seat form.
				setTimeout(() => {
					const ui = useSeatUi.getState();
					if (!ui.renaming && !ui.confirmRemove && !ui.form) focusRow(key);
				}, 0);
			}
		},
		[focusRow]
	);

	const primary = useCallback(
		(row: BoardRow) => {
			if (row.kind === 'session') {
				focusBesideBoard(boardPaneId);
				openSessionInPane(row.session.id);
				return;
			}
			const seat = row.seat;
			if (seat.status === 'vacant') {
				// D-09: a vacant row's primary act resumes its last session —
				// only where the rail's own *Resume* would (§6.2, E-1).
				const canResume =
					seat.session?.kind === 'terminal' && seat.resume.resumable && engineRunsInTerminal(seat.engine_id);
				if (canResume) void resumeSeat(seat);
				return;
			}
			if (seat.session?.kind === 'terminal') {
				focusBesideBoard(boardPaneId);
				openSessionInPane(seat.session.terminal_id);
			}
		},
		[boardPaneId]
	);

	function onGridKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
		const rowEl = (e.target as HTMLElement).closest<HTMLElement>('[role="row"][data-row-key]');
		if (!rowEl || e.target !== rowEl) return;
		const key = rowEl.dataset.rowKey;
		const row = rows.find((r) => rowKey(r) === key);
		if (!row) return;
		if (e.key === 'ArrowDown' || e.key === 'ArrowUp' || e.key === 'Home' || e.key === 'End') {
			if (e.altKey || e.metaKey || e.ctrlKey) return;
			e.preventDefault();
			const next = moveSelection(rows, selectionOf(row), e.key as 'ArrowDown' | 'ArrowUp' | 'Home' | 'End');
			if (next) {
				selectBoardRow(next);
				setTimeout(() => focusRow(rowKey(next)), 0);
			}
		} else if (e.key === 'Enter') {
			e.preventDefault();
			primary(row);
		} else if (e.key === ' ') {
			e.preventDefault();
			select(row);
		} else if (e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10')) {
			e.preventDefault();
			select(row);
			const r = rowEl.getBoundingClientRect();
			openMenu(rowKey(row), r.left + 24, r.bottom - 4);
		}
	}

	const selectedVacant = selRow?.kind === 'seat' && selRow.seat.status === 'vacant';
	const dataState = boardState({
		state,
		seatCount: seats.length,
		formOpen,
		moving: moved.size > 0,
		selectedVacant,
	});
	const empty = state === 'ready' && seats.length === 0;
	const seatable = unseated.filter(canSeatSession);
	const emptySeatTarget =
		(sel?.kind === 'session' ? seatable.find((u) => u.id === sel.id) : undefined) ?? seatable[0];
	const iyke = boardIyke(selRow, projectId);

	function rowProps(row: BoardRow): RowCommon {
		const key = rowKey(row);
		const ref = row.kind === 'seat' ? seatSessionRef(row.seat) : row.session.id;
		return {
			selected: key === selKey,
			focusable: key === tabStopKey,
			mount: mounts.get(key) ?? { where: 'none' },
			moved: moved.has(key),
			pending: ref ? (pendingBySession[ref] ?? 0) : 0,
			isTarget:
				row.kind === 'seat'
					? target.kind === 'seat' && target.seat_id === row.seat.id
					: target.kind === 'session' && target.session_id === row.session.id,
			onSelect: () => select(row),
			onPrimary: () => primary(row),
			onMenu: (x, y) => openMenu(key, x, y),
		};
	}

	return (
		<div ref={rootRef} className="chi-board" data-state={dataState} aria-label="Seat board">
			<div className="vhead">
				<h1>Seats</h1>
				<span className="note">
					{projectLabel} · {countLine(seats, unseated.length)}
				</span>
				<span className="rt">
					{seats.length > 0 && (
						<button type="button" className="btn primary" data-board-new-seat="" onClick={() => openSeatForm()}>
							<Plus aria-hidden="true" />
							<span>New seat</span>
						</button>
					)}
				</span>
			</div>

			<div className="split">
				<div className="listcol">
					{/* biome-ignore lint/a11y/useSemanticElements: an ARIA grid of rows, not a table */}
					<div
						className="seatscroll"
						role="grid"
						aria-label={`Seats in ${projectLabel}`}
						aria-busy={state === 'loading' || undefined}
						onKeyDown={onGridKeyDown}
					>
						{state === 'error' && (
							<p role="status" className="nothing">
								Seats couldn’t be read{error ? ` — ${error}` : ''}.
							</p>
						)}
						{empty ? (
							<div className="bempty" data-board-empty="">
								<p>A seat keeps an agent’s name when its pane moves or its session ends.</p>
								<div className="acts">
									<button type="button" className="btn primary" data-board-empty-new="" onClick={() => openSeatForm()}>
										<Plus aria-hidden="true" />
										<span>New seat</span>
									</button>
									<button
										type="button"
										className="btn"
										data-board-empty-seat=""
										disabled={!emptySeatTarget}
										title={
											emptySeatTarget
												? `Seat ${sessionName(emptySeatTarget.id)}`
												: 'No open sessions to seat'
										}
										onClick={() => emptySeatTarget && openSeatForm({ seatSession: emptySeatTarget.id })}
									>
										<AtSign aria-hidden="true" />
										<span>Seat this session…</span>
									</button>
								</div>
							</div>
						) : (
							seats.length > 0 && (
								<>
									<div className="shead sgrid" role="row">
										<span className="c-name" role="columnheader">
											Seat
										</span>
										<span className="c-eng" role="columnheader">
											Engine
										</span>
										<span className="c-sess" role="columnheader">
											Session
										</span>
										<span className="c-state" role="columnheader">
											State
										</span>
										<span className="c-ctx num" role="columnheader" title="Context, tokens">
											Ctx
										</span>
										<span className="c-cost num" role="columnheader">
											Cost
										</span>
										<span className="c-pend" role="columnheader" title="Pending permissions and inbox">
											Pending
										</span>
										<span className="c-pad" role="columnheader">
											Scratchpad · latest
										</span>
										<span className="c-mnt" role="columnheader">
											Mounted
										</span>
										<span className="c-act" />
									</div>
									{seats.map((seat) => (
										<SeatBoardRow key={seat.id} seat={seat} {...rowProps({ kind: 'seat', seat })} />
									))}
								</>
							)
						)}
						{state === 'ready' && (
							<>
								<div className="bgroup" role="presentation">
									Unseated sessions <span className="n">{unseated.length}</span>
									<span className="h">addressed by terminal id</span>
								</div>
								{unseated.map((session) => (
									<SessionBoardRow
										key={session.id}
										session={session}
										{...rowProps({ kind: 'session', session })}
									/>
								))}
								{unseated.length === 0 && <div className="nothing">Every open session has a seat.</div>}
							</>
						)}
					</div>
				</div>
				{selRow && (
					<div className="detailcol" data-board-detail={selKey ?? ''}>
						{selRow.kind === 'seat' ? (
							<SeatDetail
								seat={selRow.seat}
								mount={mounts.get(rowKey(selRow)) ?? { where: 'none' }}
								isTarget={target.kind === 'seat' && target.seat_id === selRow.seat.id}
								pending={(() => {
									const ref = seatSessionRef(selRow.seat);
									return ref && selRow.seat.status !== 'vacant' ? (pendingBySession[ref] ?? 0) : 0;
								})()}
								projectLabel={projectLabel}
								boardPaneId={boardPaneId}
								onMenu={(x, y) => openMenu(rowKey(selRow), x, y)}
							/>
						) : (
							<SessionDetail
								session={selRow.session}
								mount={mounts.get(rowKey(selRow)) ?? { where: 'none' }}
								isTarget={target.kind === 'session' && target.session_id === selRow.session.id}
								projectLabel={projectLabel}
							/>
						)}
					</div>
				)}
			</div>

			<div className="iykeline">
				<TerminalSquare aria-hidden="true" className="size-3" />
				<b>iyke</b>
				<span className="cmd" data-iyke-line="" title={`iyke ${iyke}`}>
					{iyke}
				</span>
				<button
					type="button"
					className="cp"
					data-board-iyke-copy=""
					aria-label="Copy the iyke command"
					onClick={() => copyText(`iyke ${iyke}`, `Copied iyke ${iyke}`)}
				>
					Copy
				</button>
			</div>

			{menu && (
				<BoardMenu
					menu={menu}
					rows={rows}
					mounts={mounts}
					boardPaneId={boardPaneId}
					onClose={closeMenu}
				/>
			)}
			{/* The Companion hosts the Remove confirm in its expanded and
			    collapsed states; while it is hidden, the board does. */}
			{companionState === 'hidden' && <SeatRemoveDialog roster={roster} />}
		</div>
	);
}

/** The rail's items that place a view in "the focused pane" — asked from the
 *  board, they are pointed at the pane beside it first. *All seats* is not
 *  one: the board is already open, and the pane store's cross-pane reuse
 *  lands it back on the board's own tab. Every other item (target, copy,
 *  rename, end, remove, pop out) leaves pane focus alone — moving it would
 *  make the router follow the other pane for nothing (and leave focus there
 *  when the item hands focus on, as Rename… and Remove seat… do). */
export function opensInPane(label: string): boolean {
	return label.startsWith('Open in pane') || label === 'Open scratchpad' || label === 'Move to pane';
}

/** The rail's own seat / session menu, opened from the board. Only the items
 *  that open something in a pane are pointed at the pane beside the board
 *  (`opensInPane`), and *Rename…* brings the Companion forward: the rename
 *  field is the rail's (D-09 "Rename… goes to the rail's inline rename and
 *  the board follows"). */
function BoardMenu({
	menu,
	rows,
	mounts,
	boardPaneId,
	onClose,
}: {
	menu: MenuState;
	rows: readonly BoardRow[];
	mounts: Map<string, Mount>;
	boardPaneId: string | null;
	onClose: (restoreFocus: boolean) => void;
}) {
	const target = useShellStore((s) => s.companion.activeTarget);
	const tabs = useTerminalStore((s) => s.tabs);
	const row = rows.find((r) => rowKey(r) === menu.key);
	// WP-69 (§4.4): a persistent-run seat's Open in pane / Pop out attach to
	// its tmux session, so the rail's menu builder needs the run's attach state.
	const runAttach = useRunAttachedTerminal(
		row?.kind === 'seat' && row.seat.session?.kind === 'run'
			? { runId: row.seat.session.run_id, engineId: row.seat.engine_id }
			: null
	);
	if (!row) return null;
	const mount = mounts.get(menu.key) ?? { where: 'none' as const };
	let label: string;
	let items: SeatMenuItem[];
	if (row.kind === 'seat') {
		const seat = row.seat;
		const tid = seat.session?.kind === 'terminal' ? seat.session.terminal_id : null;
		const live = tid ? tabs.some((t) => t.id === tid && t.status === 'running') : false;
		const isTarget = target.kind === 'seat' && target.seat_id === seat.id;
		label = `Seat actions for @${seat.name}`;
		items = seatMenuItems(seat, isTarget, mount, live, runAttach.state);
	} else {
		const isTarget = target.kind === 'session' && target.session_id === row.session.id;
		label = `Session actions for ${sessionName(row.session.id)}`;
		items = sessionMenuItems(row.session, isTarget, mount);
	}
	const wrapped = items.map((item): SeatMenuItem => {
		if (item.sep) return item;
		if (opensInPane(item.label)) {
			return {
				...item,
				run: () => {
					focusBesideBoard(boardPaneId);
					item.run();
				},
			};
		}
		if (item.label === 'Rename…') {
			return {
				...item,
				run: () => {
					useCompanionStore.getState().setState('expanded');
					item.run();
				},
			};
		}
		return item;
	});
	return <SeatMenu label={label} x={menu.x} y={menu.y} items={wrapped} onClose={onClose} />;
}
