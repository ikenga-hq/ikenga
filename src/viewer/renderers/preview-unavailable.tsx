// Honest empty state for the renderers that need the desktop's local viewer
// server (HTML, audio, video). A browser session cannot reach that server —
// its `http://localhost:<port>` would be the browser's own machine — so the
// pane says so instead of showing a broken iframe or media element (2026-10-06
// gap audit, rank 8). Remove once the daemon serves an authed viewer route.

import { MonitorOff } from 'lucide-react';

export const PREVIEW_UNAVAILABLE_BROWSER = 'Preview not available in the browser yet';

export function PreviewUnavailable({ name }: { name?: string }) {
	return (
		<div
			role="status"
			data-testid="preview-unavailable"
			className="flex h-full flex-col items-center justify-center gap-2 p-6 text-center text-muted-foreground"
		>
			<MonitorOff className="h-6 w-6 opacity-50" aria-hidden="true" />
			<div className="text-sm font-medium">{PREVIEW_UNAVAILABLE_BROWSER}</div>
			<div className="max-w-sm text-xs">
				{name ? <code>{name}</code> : 'This file'} can be previewed in the Ikenga desktop app.
			</div>
		</div>
	);
}
