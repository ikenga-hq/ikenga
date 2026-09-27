// WP-67 — *Remove seat…* confirm (D-09 rule 3, G-102; G-SEATS §4.2, §14 N-1).
//
// The dialog is exactly as the locked file draws it: the address stops
// resolving, the scratchpad and session history go with the seat, a running
// session keeps running unseated, and there are eight seconds to undo. The
// pad going is N-1's default (`removeMemory: true`); `seats_remove` takes
// either value, so if the user decides otherwise only this copy and the one
// flag in `seat-actions.ts` change.

import { Trash2 } from 'lucide-react';
import { useEffect, useRef } from 'react';
import { Dialog, DialogContent, DialogDescription, DialogTitle } from '@/components/ui/dialog';
import type { SeatView } from '@/lib/tauri-cmd';
import { removeSeat, useSeatUi } from './seat-actions';
import type { SeatRoster } from './seat-roster';
import { nextAfterRemove } from './seat-rail';
import { sessionName } from './seat-sessions';

export function SeatRemoveDialog({ roster }: { roster: SeatRoster }) {
	const seatId = useSeatUi((s) => s.confirmRemove);
	const seat = seatId ? roster.seats.find((s) => s.id === seatId) : undefined;
	const close = () => useSeatUi.setState({ confirmRemove: null });
	return (
		<Dialog open={Boolean(seat)} onOpenChange={(open) => !open && close()}>
			{seat && <RemoveBody seat={seat} roster={roster} onKeep={close} />}
		</Dialog>
	);
}

function RemoveBody({ seat, roster, onKeep }: { seat: SeatView; roster: SeatRoster; onKeep: () => void }) {
	const yes = useRef<HTMLButtonElement | null>(null);
	useEffect(() => {
		yes.current?.focus();
	}, []);
	const n = seat.pad.count;
	const running = seat.status !== 'vacant' && seat.session;
	const runningName = running
		? sessionName(seat.session?.kind === 'terminal' ? seat.session.terminal_id : (seat.session?.run_id ?? ''))
		: null;
	return (
		<DialogContent
			showCloseButton={false}
			// Radix owns `data-state` (open/closed) on the content element.
			data-seat-dialog="remove"
			className="max-w-md gap-3 p-5"
			style={{ background: 'var(--bg-surface)', borderColor: 'var(--border-strong)' }}
		>
			<div className="flex items-center gap-2">
				<Trash2 className="h-4 w-4" aria-hidden="true" style={{ color: 'var(--color-text-danger)' }} />
				<DialogTitle className="text-[16px]" style={{ color: 'var(--fg)' }}>
					Remove seat @{seat.name}
				</DialogTitle>
			</div>
			<DialogDescription asChild>
				<div className="space-y-2 text-[13px] leading-[1.55]" style={{ color: 'var(--fg)' }}>
					<p>
						Remove seat <b>@{seat.name}</b>? The address stops resolving, so{' '}
						<span className="font-mono">iyke terminal-send --seat {seat.name}</span> will fail.
					</p>
					<p>
						Its scratchpad <span className="font-mono">{seat.address}</span> ({n}{' '}
						{n === 1 ? 'entry' : 'entries'}) and its session history go with it.{' '}
						{runningName ? (
							<>
								<b>{runningName}</b> keeps running, unseated.
							</>
						) : (
							'No session is running in it.'
						)}{' '}
						You can undo for eight seconds.
					</p>
				</div>
			</DialogDescription>
			<div className="mt-1 flex items-center gap-2">
				<button
					ref={yes}
					type="button"
					onClick={() => removeSeat(seat, nextAfterRemove(seat, roster))}
					className="h-9 rounded-md border px-4 text-[13px] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
					style={{ borderColor: 'var(--color-border-danger)', color: 'var(--color-text-danger)' }}
				>
					Remove seat
				</button>
				<button
					type="button"
					onClick={onKeep}
					className="h-9 rounded-md border px-4 text-[13px] text-[var(--fg)] hover:bg-[var(--bg-raised)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
					style={{ borderColor: 'var(--border)' }}
				>
					Keep it
				</button>
				<span className="ml-auto text-[11px]" style={{ color: 'var(--fg-muted)' }}>
					Esc keeps it
				</span>
			</div>
		</DialogContent>
	);
}
