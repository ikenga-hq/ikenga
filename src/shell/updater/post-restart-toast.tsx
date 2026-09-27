// The post-restart "Updated to X" toast
// (`designs/system-flows.html?state=update-flow` step 4 / `upRestart()`:
// `toast('Updated to 0.9.1 · 2 sessions resumed', { ms: 4000 })`).
//
// There's no notification-table/toast bus yet (that's WP-40b, unmerged here —
// do-not-touch per the brief); this mounts the shared `FloatingToastChip`
// primitive directly, driven by the cross-restart marker in
// `src/lib/updater/post-restart.ts`. Mount once, near the top of the tree
// (`workspace.tsx`, alongside `<BannerSlot />`) — it renders nothing until a
// marker from *this exact version* is found on boot.

import { CheckCircle2 } from 'lucide-react';
import { useEffect, useState } from 'react';
import { FloatingToastChip } from '@/components/ui/floating-toast-chip';
import { consumePendingRestartIfMatching, type PendingRestart } from '@/lib/updater/post-restart';
import { useLiveSessionCount } from '@/lib/updater/restart-sessions';

export function PostRestartUpdateToast() {
	const [pending, setPending] = useState<PendingRestart | null>(null);
	const liveNow = useLiveSessionCount();

	useEffect(() => {
		let cancelled = false;
		void consumePendingRestartIfMatching().then((result) => {
			if (!cancelled && result) setPending(result);
		});
		return () => {
			cancelled = true;
		};
	}, []);

	if (!pending) return null;

	// `sessionsBefore` is what was live right before the restart was
	// requested; `liveNow` is the terminal store's real post-boot count, which
	// reflects however many actually came back (persistent tabs rehydrate on
	// boot). Reporting the live figure rather than echoing the pre-restart one
	// keeps the toast honest if a session didn't make it back.
	const resumed = Math.min(pending.sessionsBefore, liveNow);

	return (
		<div data-state="update-updated">
			<FloatingToastChip
				variant="notice"
				anchor="viewport-top"
				icon={<CheckCircle2 />}
				ttlMs={4000}
				onDismiss={() => setPending(null)}
				label={
					<span>
						Updated to <span className="font-mono">{pending.version}</span>
						{pending.sessionsBefore > 0 && (
							<span className="text-muted-foreground">
								{' '}
								· {resumed} of {pending.sessionsBefore} session
								{pending.sessionsBefore === 1 ? '' : 's'} resumed
							</span>
						)}
					</span>
				}
			/>
		</div>
	);
}
