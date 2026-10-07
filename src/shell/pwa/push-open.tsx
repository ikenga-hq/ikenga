// plans/pwa S4 §7: act on a notification tap once the app is up — the tap
// captured from the URL at boot (`deeplink-capture.ts`) and any tap posted by
// the service worker to this already-open window — and say so when the thing
// was already dealt with elsewhere.

import { BellRing } from 'lucide-react';
import { useEffect } from 'react';

import { Banner } from '@/components/ui/banner';
import { Button } from '@/components/ui/button';
import {
	handlePushOpen,
	onPushOpenMessage,
	takePushDeepLink,
	usePushOpenStore,
} from '@/lib/pwa/deeplink';
import { isTauri } from '@/lib/transport';

/** Consume taps; `navigate` moves the workspace (omitted on `/remote`). */
export function usePushOpenHandling(navigate?: (path: string) => void): void {
	useEffect(() => {
		if (isTauri()) return;
		const first = takePushDeepLink();
		// The tap's URL already opened the right route; only look it up.
		if (first) void handlePushOpen(first);
		return onPushOpenMessage((msg) => void handlePushOpen(msg, { navigate }));
	}, [navigate]);
}

/** The "already answered elsewhere" line, if any. */
export function PushOpenNotice({ className }: { className?: string }) {
	const notice = usePushOpenStore((s) => s.notice);
	const clear = usePushOpenStore((s) => s.setNotice);
	if (!notice) return null;
	return (
		<p
			role="status"
			data-state="push-open-notice"
			className={`m-0 flex items-center gap-2 text-[length:var(--text-micro)] text-[var(--fg-muted)] ${className ?? ''}`}
		>
			<BellRing aria-hidden className="size-3.5 flex-none" />
			<span className="min-w-0 flex-1">{notice}</span>
			<button type="button" className="underline" onClick={() => clear(null)}>
				OK
			</button>
		</p>
	);
}

/** Banner-slot entry (`info` tier) for the workspace. */
export function PushOpenBanner() {
	const notice = usePushOpenStore((s) => s.notice);
	const clear = usePushOpenStore((s) => s.setNotice);
	usePushOpenHandling(navigateWorkspace);
	if (!notice) return null;
	return (
		<Banner
			data-state="push-open"
			tone="info"
			icon={<BellRing />}
			actions={
				<Button size="sm" variant="outline" onClick={() => clear(null)}>
					OK
				</Button>
			}
		>
			{notice}
		</Banner>
	);
}

function navigateWorkspace(path: string): void {
	void import('@/lib/panes/pane-store').then((m) =>
		m.usePaneStore.getState().navigateFocused(path)
	);
}
