// D-08 pane chrome — "viewer server stopped" detection
// (designs/pane-chrome.html?state=artifact-stopped).
//
// Only HTML artifacts are backed by a real server: `HtmlFrame` mounts the
// shared axum viewer server via `viewer_serve` (src-tauri/src/commands/
// viewer.rs). Every other renderer reads the file straight off disk
// (`fsRead`) — there is no "server" for them to lose, so this hook is a
// no-op (`stopped: false`) for non-HTML paths.
//
// `viewer_port` returns `null` when the shared server failed to bind
// (src/lib/tauri-cmd.ts `viewerPort`) — that's the one signal available
// without adding a new Rust command, and it's the honest one: the server is
// shell-wide, not per-artifact, so "stopped" here means the whole viewer
// server is down, not just this file's mount.

import { useCallback, useEffect, useState } from 'react';
import { viewerPort } from '@/lib/tauri-cmd';
import { isHtmlArtifactPath } from '../lib/viewer-url';

export interface ViewerServerHealth {
	stopped: boolean;
	/** Re-checks the server and, on success, clears `stopped` so the caller
	 *  can remount its renderer. */
	restart: () => Promise<void>;
}

export function useViewerServerHealth(path: string): ViewerServerHealth {
	const applies = isHtmlArtifactPath(path);
	const [stopped, setStopped] = useState(false);

	const check = useCallback(async () => {
		if (!applies) {
			setStopped(false);
			return;
		}
		try {
			const port = await viewerPort();
			setStopped(port === null);
		} catch {
			setStopped(true);
		}
	}, [applies]);

	useEffect(() => {
		void check();
	}, [check]);

	return { stopped, restart: check };
}
