// WP-67 — a vacant seat, selected (D-09 `vacant` state; G-SEATS §2.2, §4.1,
// §4.2, §6.2, §9.3). What is left of it — its last session, when it was last
// active, the context it ended with, the scratchpad's latest entry — and three
// ways on: **Resume session N** (primary), **Fill with a new session**,
// **Clear seat** (keeps the pad, 8 s Undo).
//
// An explicit Resume never falls back to a fresh start (§6.2): when the seat
// can't resume, the button is disabled and says why, and Fill is the way on.

import { cn } from '@/components/ui/utils';
import type { SeatView } from '@/lib/tauri-cmd';
import { clearSeat, copyText, fillSeat, openSeatScratchpad, resumeSeat } from './seat-actions';
import { engineResumeFlag, iykeVacant, notResumableText, padText, seatSessionRef, UNREPORTED } from './seat-model';
import { formatSeatTime } from './seat-notice';
import { sessionName, useSessionFigures } from './seat-sessions';

function relativeTime(ms: number, now = Date.now()): string {
	const s = Math.max(0, Math.round((now - ms) / 1000));
	if (s < 60) return 'just now';
	const m = Math.round(s / 60);
	if (m < 60) return `${m}m ago`;
	const h = Math.round(m / 60);
	if (h < 24) return `${h}h ago`;
	return `${Math.round(h / 24)}d ago`;
}

function Row({ k, children, mono, title }: { k: string; children: React.ReactNode; mono?: boolean; title?: string }) {
	return (
		<div className="flex min-h-6 items-baseline gap-2 text-[13px]">
			<span className="w-[108px] shrink-0 text-[11px]" style={{ color: 'var(--fg-muted)' }}>
				{k}
			</span>
			<span className={cn('min-w-0 truncate', mono && 'font-mono')} style={{ color: 'var(--fg)' }} title={title}>
				{children}
			</span>
		</div>
	);
}

export function SeatVacantPanel({ seat }: { seat: SeatView }) {
	const ref = seatSessionRef(seat);
	const figures = useSessionFigures(ref);
	const last = ref ? sessionName(ref) : null;
	const flag = engineResumeFlag(seat.engine_resume);
	const resumable = seat.resume.resumable;
	const reason = seat.resume.resumable ? null : notResumableText(seat.resume.reason);
	const iyke = iykeVacant(seat.name, Boolean(seat.session) && resumable);

	return (
		<section
			aria-label={`Vacant @${seat.name}`}
			data-state="seats-vacant"
			className="shrink-0 border-b"
			style={{ borderColor: 'var(--border-soft)' }}
		>
			<div className="flex h-7 items-center gap-2 px-3">
				<span className="text-[11px] font-semibold uppercase tracking-widest" style={{ color: 'var(--fg-muted)' }}>
					Vacant
				</span>
				<span className="font-mono text-[11px]" style={{ color: 'var(--fg)' }}>
					@{seat.name}
				</span>
				<button
					type="button"
					onClick={() => openSeatScratchpad(seat)}
					title={`Open scratchpad ${seat.address}`}
					className="ml-auto truncate font-mono text-[11px] text-[var(--fg-muted)] hover:text-[var(--fg)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
				>
					{seat.address}
				</button>
			</div>
			<div className="px-3 pb-3">
				<Row k="Last session" mono>
					{last ? `${last} · ${seat.engine_id}` : 'none — cleared'}
				</Row>
				{seat.session && (
					<>
						<Row k="Last active" title={formatSeatTime(seat.last_active_at)}>
							{relativeTime(seat.last_active_at)}
						</Row>
						<Row k="Context at end" mono title={figures.ctx ? undefined : UNREPORTED}>
							{figures.ctx ?? '—'}
						</Row>
					</>
				)}
				<Row k="Scratchpad">{padText(seat)}</Row>
				{flag && <Row k="Engine">{flag}</Row>}
				<div className="mb-2 mt-3 flex flex-wrap gap-2">
					{seat.session && (
						<button
							type="button"
							disabled={!resumable}
							title={reason ?? `Resume ${last ?? 'its last session'} in @${seat.name}`}
							onClick={() => void resumeSeat(seat)}
							className="h-7 rounded-md bg-[var(--primary)] px-3 text-xs text-[var(--primary-fg)] hover:opacity-90 disabled:cursor-not-allowed disabled:bg-[var(--bg-raised)] disabled:text-[var(--fg-muted)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
						>
							Resume {last ?? 'session'}
						</button>
					)}
					<button
						type="button"
						onClick={() => void fillSeat(seat)}
						className={cn(
							'h-7 rounded-md px-3 text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
							seat.session && resumable
								? 'border text-[var(--fg)] hover:bg-[var(--bg-raised)]'
								: 'bg-[var(--primary)] text-[var(--primary-fg)] hover:opacity-90'
						)}
						style={seat.session && resumable ? { borderColor: 'var(--border)' } : undefined}
					>
						Fill with a new session
					</button>
					<button
						type="button"
						disabled={!seat.session}
						title={seat.session ? 'Forget its session history; the scratchpad stays' : 'Already cleared'}
						onClick={() => clearSeat(seat)}
						className="h-7 rounded-md px-3 text-xs text-[var(--fg-muted)] hover:bg-[var(--bg-raised)] hover:text-[var(--fg)] disabled:cursor-not-allowed disabled:opacity-60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
					>
						Clear seat
					</button>
				</div>
				{reason && seat.session && (
					<p className="mb-2 text-[11px]" style={{ color: 'var(--fg-muted)' }}>
						Can’t resume: {reason}. Fill it with a new session instead.
					</p>
				)}
				<div
					className="-mx-3 -mb-3 flex h-7 items-center gap-2 border-t px-3 font-mono text-[11px]"
					style={{ borderColor: 'var(--border-soft)', color: 'var(--fg-muted)' }}
				>
					<b className="font-semibold" style={{ color: 'var(--fg)' }}>
						iyke
					</b>
					<span className="min-w-0 flex-1 truncate" style={{ color: 'var(--fg)' }}>
						{iyke}
					</span>
					<button
						type="button"
						onClick={() => copyText(`iyke ${iyke}`, `Copied iyke ${iyke}`)}
						className="shrink-0 rounded-sm px-1 hover:text-[var(--fg)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
					>
						Copy
					</button>
				</div>
			</div>
		</section>
	);
}
