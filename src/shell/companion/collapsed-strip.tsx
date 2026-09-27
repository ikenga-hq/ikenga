// The Companion at rest — spec §5.1, #55–#57, reworked for D-09's `rest`
// state (WP-67). A 36 px strip that carries one monogram per seat (`le`,
// `re`, …) with its state dot on the corner and a pending permission as a
// badge, then the unseated sessions (dashed, `s4`), then the vertical label
// `Chi · <state>` and the expand button. The two signals that must survive
// here (D-09 `rest`): a pending permission on a seat, and a run's pulse.
//
// A request no monogram here carries (no terminal id, or a session with no
// monogram on this strip) still shows as the shield glyph, counted, so the
// pending signal always survives at rest. It accepts tab drops.

import { ChevronLeft, ShieldAlert } from 'lucide-react';
import { cn } from '@/components/ui/utils';
import { labelFor } from '@/lib/keymap/registry';
import type { DropTargetProps } from '@/lib/panes/pointer-drag';
import type { SeatStatus, SeatView } from '@/lib/tauri-cmd';
import { atName, seatMonogram, seatSessionRef, stateDotColor } from './seat-model';
import type { UnseatedSession } from './seat-roster';
import { sessionName, sessionNumber } from './seat-sessions';

export const COLLAPSED_WIDTH = 36;

export interface CollapsedStripProps {
	/** Every pending permission request (the label and accessible name). */
	pendingPermissions: number;
	/** A session in the Companion is running (the label, when nothing is pending). */
	live: boolean;
	onExpand: () => void;
	/** The roster's seats, in rail order. */
	seats?: readonly SeatView[];
	unseated?: readonly UnseatedSession[];
	/** Pending requests per session id (terminal id). */
	pendingBySession?: Readonly<Record<string, number>>;
	/** Requests pinned on no session. The shield glyph shows every request no
	 *  monogram carries (these, plus any on a session with no monogram). */
	pendingUnattributed?: number;
	/** The selected rail row's key (`seat:<id>` / `session:<id>`). */
	selectedKey?: string | null;
	onPickSeat?: (seat: SeatView, toPermission: boolean) => void;
	onPickSession?: (id: string, toPermission: boolean) => void;
	dropProps?: DropTargetProps;
	dropHover?: boolean;
}

/** `Chi · 1 pending` / `Chi · live` / `Chi`. */
export function stripStateLabel(pending: number, live: boolean): string {
	if (pending > 0) return `Chi · ${pending} pending`;
	if (live) return 'Chi · live';
	return 'Chi';
}

/** §5.1 accessible name: "Chi companion, collapsed, 1 permission pending.
 *  Expand (⌘J)." — the key label comes from the keymap registry. */
export function stripAccessibleName(pending: number, live: boolean): string {
	const parts = ['Chi companion', 'collapsed'];
	if (pending > 0) parts.push(`${pending} permission${pending === 1 ? '' : 's'} pending`);
	else if (live) parts.push('a session is live');
	const key = labelFor('companion.toggle');
	return `${parts.join(', ')}. Expand${key ? ` (${key})` : ''}.`;
}

function CornerDot({ status }: { status: SeatStatus }) {
	return (
		<span
			aria-hidden="true"
			data-dot={status}
			className={cn(
				'absolute -bottom-[3px] -right-[3px] size-2 rounded-full',
				status === 'run' && 'motion-safe:animate-pulse'
			)}
			style={{
				background: status === 'vacant' ? 'var(--bg-base)' : stateDotColor(status),
				boxShadow:
					status === 'vacant'
						? 'inset 0 0 0 1.5px var(--fg-muted), 0 0 0 2px var(--bg-base)'
						: '0 0 0 2px var(--bg-base)',
			}}
		/>
	);
}

function Badge({ n }: { n: number }) {
	return (
		<span
			aria-hidden="true"
			data-attention="permission"
			className="absolute -right-1.5 -top-1.5 h-[15px] min-w-[15px] rounded-full px-0.5 text-center font-mono text-[11px] leading-[15px]"
			style={{ background: 'var(--achievement)', color: 'var(--live-fg)', boxShadow: '0 0 0 2px var(--bg-base)' }}
		>
			{n}
		</span>
	);
}

const MONO_CLASS =
	'relative grid size-[26px] place-items-center rounded-[var(--radius-sm)] border font-mono text-[11px] hover:text-[var(--fg)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring';

export function CollapsedStrip({
	pendingPermissions,
	live,
	onExpand,
	seats = [],
	unseated = [],
	pendingBySession = {},
	pendingUnattributed,
	selectedKey = null,
	onPickSeat,
	onPickSession,
	dropProps,
	dropHover,
}: CollapsedStripProps) {
	// What each monogram carries as a badge: a seat shows its session's
	// requests while it holds one (not vacant); an unseated session shows its own.
	const seatPending = (seat: SeatView): number => {
		const ref = seatSessionRef(seat);
		return ref && seat.status !== 'vacant' ? (pendingBySession[ref] ?? 0) : 0;
	};
	const drawn =
		seats.reduce((n, seat) => n + seatPending(seat), 0) +
		unseated.reduce((n, u) => n + (pendingBySession[u.id] ?? 0), 0);
	// Every request no monogram carries — unattributed, or on a session with
	// no monogram here (a plain terminal, another project's seat, a vacant
	// seat's old session) — shows as the shield, so none is lost at rest
	// (D-09 `rest`). Without a per-session breakdown, all of them.
	const hasBreakdown = pendingUnattributed !== undefined || seats.length > 0 || unseated.length > 0;
	const loose = hasBreakdown ? Math.max(0, pendingPermissions - drawn) : pendingPermissions;
	const monograms = seats.length + unseated.length;
	return (
		<aside
			aria-label="Chi companion"
			data-state="seats-rest"
			className={cn(
				'flex h-full shrink-0 flex-col items-center gap-2 border-l py-2',
				dropHover ? 'bg-[var(--primary-soft)]' : 'bg-[var(--bg-base)]'
			)}
			style={{ width: `${COLLAPSED_WIDTH}px`, borderColor: 'var(--border-soft)' }}
			{...dropProps}
		>
			{monograms > 0 && (
				// biome-ignore lint/a11y/useSemanticElements: a labelled group of buttons, not a form fieldset
				<div role="group" aria-label="Seats" className="flex flex-col items-center gap-2 pt-1">
					{seats.map((seat) => {
						const pending = seatPending(seat);
						const on = selectedKey === `seat:${seat.id}`;
						const state =
							seat.status === 'live' ? 'live' : seat.status === 'run' ? 'run' : seat.status === 'idle' ? 'idle' : 'vacant';
						const name = `${atName(seat.name)} · ${state}${pending ? ` · ${pending} permission${pending === 1 ? '' : 's'} pending` : ''}${seat.inbox_count && seat.status !== 'vacant' ? ` · inbox ${seat.inbox_count}` : ''}`;
						return (
							<button
								key={seat.id}
								type="button"
								data-mono={seat.name}
								title={name}
								aria-label={`${name}. Expand the Companion on this seat.`}
								onClick={() => onPickSeat?.(seat, pending > 0)}
								className={MONO_CLASS}
								style={{
									color: on ? 'var(--fg)' : 'var(--fg-muted)',
									borderColor: on ? 'var(--primary)' : 'var(--border-soft)',
									background: 'var(--bg-surface)',
								}}
							>
								{seatMonogram(seat.name)}
								<CornerDot status={seat.status} />
								{pending > 0 && <Badge n={pending} />}
							</button>
						);
					})}
					{seats.length > 0 && unseated.length > 0 && (
						<span aria-hidden="true" className="h-px w-4" style={{ background: 'var(--border)' }} />
					)}
					{unseated.map((u) => {
						const pending = pendingBySession[u.id] ?? 0;
						const on = selectedKey === `session:${u.id}`;
						const liveNow = u.status === 'running' || u.status === 'spawning';
						const name = `${sessionName(u.id)} · unseated${pending ? ` · ${pending} permission${pending === 1 ? '' : 's'} pending` : ''}`;
						return (
							<button
								key={u.id}
								type="button"
								data-mono-session={u.id}
								title={name}
								aria-label={`${name}. Expand the Companion on this session.`}
								onClick={() => onPickSession?.(u.id, pending > 0)}
								className={cn(MONO_CLASS, 'border-dashed')}
								style={{
									color: on ? 'var(--fg)' : 'var(--fg-muted)',
									borderColor: on ? 'var(--primary)' : 'var(--border-soft)',
								}}
							>
								{`s${sessionNumber(u.id)}`}
								<CornerDot status={liveNow ? 'live' : 'idle'} />
								{pending > 0 && <Badge n={pending} />}
							</button>
						);
					})}
				</div>
			)}
			{loose > 0 && (
				<span className="relative grid size-6 place-items-center" aria-hidden="true" data-attention="permission">
					<ShieldAlert className="h-4 w-4" style={{ color: 'var(--achievement)' }} />
					<span
						className="absolute -right-1 -top-1 min-w-3.5 rounded-full px-0.5 text-center font-mono text-[11px] leading-[14px]"
						style={{ background: 'var(--achievement)', color: 'var(--bg-base)' }}
					>
						{loose}
					</span>
				</span>
			)}
			{monograms === 0 && loose === 0 && live && (
				<span className="grid size-6 place-items-center" aria-hidden="true" data-attention="run">
					<span className="size-2 rounded-full motion-safe:animate-pulse" style={{ background: 'var(--live)' }} />
				</span>
			)}
			<span aria-hidden="true" className="text-[11px] tracking-wider [writing-mode:vertical-rl]" style={{ color: 'var(--fg-muted)' }}>
				{stripStateLabel(pendingPermissions, live)}
			</span>
			<span className="flex-1" />
			<button
				type="button"
				aria-expanded={false}
				aria-label={stripAccessibleName(pendingPermissions, live)}
				title={`Expand Companion${labelFor('companion.toggle') ? ` (${labelFor('companion.toggle')})` : ''}`}
				onClick={onExpand}
				className="grid size-6 place-items-center rounded-sm text-[var(--fg-muted)] hover:bg-[var(--bg-raised)] hover:text-[var(--fg)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
			>
				<ChevronLeft className="h-3.5 w-3.5" aria-hidden="true" />
			</button>
		</aside>
	);
}
