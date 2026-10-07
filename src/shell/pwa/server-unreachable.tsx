// plans/pwa S1 (W2): the honest offline state.
//
// The service worker can start the app shell with no network, but every byte
// of data comes live from the server (no cached API responses, no offline
// editing). When the boot probe can't reach the server at all, this says so —
// instead of the pair/sign-in overlay, which would wrongly suggest the device
// lost its credential. Retry re-runs the boot; coming back online does too.

import { WifiOff } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { Button } from '@/components/ui/button';

export interface ServerUnreachableProps {
	/** Re-run whatever failed. Defaults to a page reload (a fresh boot). */
	onRetry?: () => void;
}

function isOffline(): boolean {
	return typeof navigator !== 'undefined' && navigator.onLine === false;
}

export function ServerUnreachable({ onRetry }: ServerUnreachableProps) {
	const [offline, setOffline] = useState(isOffline);
	const retryRef = useRef(onRetry);
	retryRef.current = onRetry;
	const retry = () => (retryRef.current ?? (() => window.location.reload()))();

	// biome-ignore lint/correctness/useExhaustiveDependencies: retry reads the latest prop through a ref
	useEffect(() => {
		const goOffline = () => setOffline(true);
		const goOnline = () => {
			setOffline(false);
			retry();
		};
		window.addEventListener('offline', goOffline);
		window.addEventListener('online', goOnline);
		return () => {
			window.removeEventListener('offline', goOffline);
			window.removeEventListener('online', goOnline);
		};
	}, []);

	return (
		<div
			data-state="server-unreachable"
			role="alert"
			className="grid min-h-dvh place-items-center bg-[var(--bg-base)] px-6 text-[var(--fg)]"
		>
			<div className="flex max-w-[360px] flex-col items-center gap-3 text-center">
				<WifiOff aria-hidden className="size-6 text-[var(--fg-muted)]" />
				<h1 className="text-[length:var(--text-body)] font-semibold">
					Can't reach the Ikenga server
				</h1>
				<p className="text-[length:var(--text-body-sm)] text-[var(--fg-muted)]">
					{offline
						? 'This device is offline. Ikenga will try again when the network is back.'
						: 'The server isn’t responding right now. It may be stopped or restarting, or on a network this device can’t reach.'}
				</p>
				<Button size="sm" onClick={() => retry()}>
					Retry
				</Button>
			</div>
		</div>
	);
}
