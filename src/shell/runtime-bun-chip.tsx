// Floating, non-blocking status chip for the post-launch bun runtime fetch.
//
// The shell no longer bundles bun (B+A hybrid). On a fresh install (no system
// bun, no IKENGA_BUN_PATH) the Rust side fetches a sha-pin-verified bun after
// the window is interactive and narrates progress on `runtime://bun`. This chip
// mirrors WizardPopRecoveryChip's layout: a centered pill at top, pointer-
// events-none wrapper so it never steals clicks, pointer-events-auto pill.
//
// Returns null when there's nothing to show (no event yet, or ready). On error
// it offers a Retry button that re-runs the fetch via `runtimeRetryBunFetch`.

import { useEffect, useState } from 'react';
import { Download, RefreshCw } from 'lucide-react';

import { FloatingToastChip } from '@/components/ui/floating-toast-chip';
import { type RuntimeBunEvent, type UnlistenFn, runtimeBunListen, runtimeRetryBunFetch } from '@/lib/tauri-cmd';

export function RuntimeBunChip() {
	const [ev, setEv] = useState<RuntimeBunEvent | null>(null);

	useEffect(() => {
		let cancelled = false;
		let unlisten: UnlistenFn | null = null;
		void (async () => {
			unlisten = await runtimeBunListen((p) => {
				if (!cancelled) setEv(p);
			});
			if (cancelled) {
				unlisten();
				unlisten = null;
			}
		})();
		return () => {
			cancelled = true;
			unlisten?.();
		};
	}, []);

	if (!ev || ev.state === 'ready') return null;

	let label: string;
	switch (ev.state) {
		case 'checking':
			label = 'Preparing runtime…';
			break;
		case 'downloading':
			label = `Fetching runtime… ${ev.pct}%`;
			break;
		case 'verifying':
			label = 'Verifying runtime…';
			break;
		case 'error':
			label = 'Runtime download failed';
			break;
	}

	const isError = ev.state === 'error';

	return (
		<FloatingToastChip
			variant={isError ? 'error' : 'progress'}
			anchor="viewport-top"
			icon={<Download className="h-3 w-3" />}
			label={label}
			action={
				isError
					? {
							label: 'Retry',
							icon: <RefreshCw className="h-3 w-3" />,
							onClick: () => void runtimeRetryBunFetch(),
						}
					: undefined
			}
		/>
	);
}
