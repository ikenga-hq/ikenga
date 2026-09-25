// D-08 pane chrome — the "changed on disk" info strip
// (designs/pane-chrome.html?state=artifact, `opts.strip === 'changed'`).
//
// Watches the artifact's own file (not its directory — `html-frame.tsx`
// watches the parent because an HTML file's *siblings* can change what it
// renders; this strip is about the file itself). On a change it bumps
// `reloadKey` (remounts the renderer so it re-reads the file) and shows the
// strip for a few seconds before auto-dismissing, matching the design's "—
// reloaded (auto-dismiss)" copy. A manual dismiss (the strip's `x`) is
// always available immediately.
//
// Scope note (see WP-44 report): the mock's copy attributes the change to a
// person/session ("changed on disk 2 min ago by claude · session 3"). No
// subsystem in this codebase attaches that attribution to a raw fs-watch
// event, so this hook reports the fact of the change only.

import { useEffect, useRef, useState } from 'react';
import { fsListenWatch, fsUnwatch, fsWatch } from '@/lib/tauri-cmd';

const AUTO_DISMISS_MS = 6_000;
const DEBOUNCE_MS = 150;

export interface ArtifactDiskWatch {
	/** True while the "changed on disk — reloaded" strip should show. */
	changed: boolean;
	/** Bump this into a component `key` to force the renderer to re-mount and
	 *  re-read the file. */
	reloadKey: number;
	dismiss: () => void;
}

export function useArtifactDiskWatch(path: string): ArtifactDiskWatch {
	const [reloadKey, setReloadKey] = useState(0);
	const [changed, setChanged] = useState(false);
	const dismissTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

	useEffect(() => {
		let cancelled = false;
		let watcherId: string | null = null;
		let unlisten: (() => void) | null = null;
		let debounce: ReturnType<typeof setTimeout> | null = null;

		void (async () => {
			try {
				const id = await fsWatch(path);
				if (cancelled) {
					void fsUnwatch(id);
					return;
				}
				watcherId = id;
				unlisten = await fsListenWatch(id, () => {
					if (cancelled) return;
					if (debounce) clearTimeout(debounce);
					debounce = setTimeout(() => {
						setReloadKey((k) => k + 1);
						setChanged(true);
						if (dismissTimer.current) clearTimeout(dismissTimer.current);
						dismissTimer.current = setTimeout(() => setChanged(false), AUTO_DISMISS_MS);
					}, DEBOUNCE_MS);
				});
			} catch {
				// Best-effort — no strip if the watch itself can't be set up.
			}
		})();

		return () => {
			cancelled = true;
			if (debounce) clearTimeout(debounce);
			if (unlisten) unlisten();
			if (watcherId) void fsUnwatch(watcherId);
			if (dismissTimer.current) clearTimeout(dismissTimer.current);
		};
	}, [path]);

	return {
		changed,
		reloadKey,
		dismiss: () => {
			setChanged(false);
			if (dismissTimer.current) clearTimeout(dismissTimer.current);
		},
	};
}
