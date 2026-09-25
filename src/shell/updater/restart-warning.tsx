// The "These are live right now" list inside the restart step of
// `update-sheet.tsx` (`designs/system-flows.html?state=update-flow` step 3).
// Real data from `useLiveSessionsForRestart()` — no fixture rows.

import { useLiveSessionsForRestart } from '@/lib/updater/restart-sessions';

export function RestartWarning() {
	const sessions = useLiveSessionsForRestart();

	if (sessions.length === 0) {
		return (
			<p className="text-sm text-muted-foreground">
				Nothing is running right now — the restart won't interrupt anything.
			</p>
		);
	}

	return (
		<div className="space-y-2">
			<div className="text-xs font-medium text-muted-foreground">These are live right now</div>
			<ul className="space-y-1.5">
				{sessions.map((s) => (
					<li
						key={s.id}
						className="flex items-center justify-between gap-3 rounded-md border border-border bg-background px-3 py-1.5 text-sm"
					>
						<span className="min-w-0 truncate font-mono">{s.title}</span>
						<span
							className="shrink-0 font-mono text-[11px]"
							style={{ color: s.resumable ? 'var(--live)' : 'var(--achievement, var(--warning))' }}
						>
							{s.resumable ? 'will be resumed' : 'will be restarted from the top'}
						</span>
					</li>
				))}
			</ul>
			<p className="text-xs text-muted-foreground">
				Sessions with a captured id resume the conversation; a persistent run with no resume point
				just starts again — finish it first if that matters.
			</p>
		</div>
	);
}
