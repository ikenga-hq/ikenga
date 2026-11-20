import { useEffect, useState } from 'react';
import { isRemoteWebSession, viewerPort, viewerServe, viewerStop } from '@/lib/tauri-cmd';
import { dirname } from './path';
import { makeImageResolver, type ImageResolver } from './markdown-assets';

/**
 * Mount the document's folder on the viewer server while `enabled`, and return
 * a resolver for relative image sources. Same URL shapes as the HTML viewer:
 * the page origin in a browser session, localhost:<viewer port> on desktop.
 */
export function useDocAssetResolver(path: string, enabled: boolean): ImageResolver | undefined {
	const [base, setBase] = useState<string | null>(null);

	useEffect(() => {
		setBase(null);
		if (!enabled) return;
		let cancelled = false;
		let token: string | null = null;
		void (async () => {
			try {
				const h = await viewerServe(dirname(path), path);
				token = h.token;
				if (cancelled) {
					void viewerStop(h.token);
					return;
				}
				const origin = isRemoteWebSession()
					? window.location.origin
					: `http://localhost:${(await viewerPort()) ?? 47821}`;
				if (!cancelled) setBase(`${origin}${h.url}`);
			} catch {
				// Mount failed (path outside the allowed roots, no server): the
				// images stay unresolved rather than loading from the wrong origin.
			}
		})();
		return () => {
			cancelled = true;
			if (token) void viewerStop(token);
		};
	}, [path, enabled]);

	return enabled ? makeImageResolver(base) : undefined;
}
