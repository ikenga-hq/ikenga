// Settings → About status line under the header strip.
//
// The in-app updater (Tauri plugin-updater) is desktop-only: in a browser
// session `checkForUpdate()` always answers null, so the old "Ikenga is up to
// date. Last checked just now" line was claiming a check that never ran
// (audit 2026-10-06 rank 17). A browser session says who updates the server
// instead.

import { CheckCircle2, Info } from 'lucide-react';

export function UpdateStatus({
	desktop,
	available,
	checking,
	lastCheckedAt,
}: {
	/** True when the in-app updater can run (`isTauri()`). */
	desktop: boolean;
	available: boolean;
	checking: boolean;
	lastCheckedAt: number | null;
}) {
	if (!desktop) {
		return (
			<div
				className="flex items-center gap-3 rounded-lg border border-[var(--border-soft)] bg-card px-4 py-3 text-sm"
				data-testid="update-status-browser"
			>
				<Info className="size-4 shrink-0 text-muted-foreground" />
				<span className="text-muted-foreground">
					This browser session can't check for or install Ikenga updates — the in-app updater runs
					only in the desktop app. The server is updated by whoever runs it.
				</span>
			</div>
		);
	}
	if (available || checking) return null;
	return (
		<div
			className="flex items-center gap-3 rounded-lg border border-[var(--border-soft)] bg-card px-4 py-3 text-sm"
			data-testid="update-status-current"
		>
			<CheckCircle2 className="size-4 text-emerald-500" />
			<span className="text-muted-foreground">
				Ikenga is up to date. Last checked{' '}
				<span className="text-foreground">{formatRelative(lastCheckedAt)}</span>.
			</span>
		</div>
	);
}

export function formatRelative(ms: number | null): string {
	if (!ms) return 'never';
	const secs = Math.floor((Date.now() - ms) / 1000);
	if (secs < 30) return 'just now';
	if (secs < 60) return `${secs}s ago`;
	const mins = Math.floor(secs / 60);
	if (mins < 60) return `${mins}m ago`;
	const hours = Math.floor(mins / 60);
	if (hours < 24) return `${hours}h ago`;
	return `${Math.floor(hours / 24)}d ago`;
}
