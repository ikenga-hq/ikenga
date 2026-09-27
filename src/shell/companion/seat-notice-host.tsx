// WP-66 — renders the seat notice (`seat-notice.ts`) as the shell's floating
// toast pill. WP-67 mounts it once, at the Companion level (`companion.tsx`),
// in both the expanded and the collapsed state, so a notice raised while the
// Companion rests at its strip (an E-4 queue-dropped, a pop-out, an Undo)
// shows at once instead of waiting for the dispatch bar to mount.

import { AlertTriangle, Info } from 'lucide-react';
import { FloatingToastChip } from '@/components/ui/floating-toast-chip';
import { useSeatNotice } from './seat-notice';

/** Long enough to read a sentence; longer when there is a button to reach. */
export const SEAT_NOTICE_TTL_MS = 6_000;
export const SEAT_NOTICE_ACTION_TTL_MS = 12_000;

export function SeatNoticeHost() {
	const notice = useSeatNotice((s) => s.notice);
	const dismiss = useSeatNotice((s) => s.dismiss);
	if (!notice) return null;
	const action = notice.action;
	return (
		<FloatingToastChip
			key={notice.seq}
			variant={notice.variant === 'error' ? 'error' : 'info'}
			icon={notice.variant === 'error' ? <AlertTriangle /> : <Info />}
			label={notice.message}
			action={
				action
					? {
							label: action.label,
							onClick: () => {
								dismiss();
								void action.run();
							},
						}
					: undefined
			}
			onDismiss={dismiss}
			ttlMs={notice.ttlMs ?? (action ? SEAT_NOTICE_ACTION_TTL_MS : SEAT_NOTICE_TTL_MS)}
		/>
	);
}
