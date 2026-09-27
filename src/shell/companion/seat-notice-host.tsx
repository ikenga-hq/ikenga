// WP-66 — renders the seat-dispatch notice (`seat-notice.ts`) as the shell's
// floating toast pill. Mounted once, by the dispatch bar. A notice raised
// while the Companion is collapsed or hidden (an E-4 queue-dropped from a
// runner action, say) stays in the store and shows when the bar mounts;
// mounting the host at the Companion level is WP-67's (it owns
// `companion.tsx` / `collapsed-strip.tsx`).

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
			ttlMs={action ? SEAT_NOTICE_ACTION_TTL_MS : SEAT_NOTICE_TTL_MS}
		/>
	);
}
