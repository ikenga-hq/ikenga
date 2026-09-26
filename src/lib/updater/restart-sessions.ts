// Live-session accounting for the "Restart to finish" warning
// (`designs/system-flows.html` `update-flow` step 3: "These are live right
// now" — 2 live sessions will be resumed). Reads the real terminal session
// store (`src/terminal/session-store.ts`), the same source `status-bar.tsx`'s
// `useLiveRunIds()` reads from, rather than inventing session data.
//
// A tab only carries state across the restart when it's daemon-backed
// (`mode === 'persistent'`, contract G-01 in session-store.ts). Whether it
// comes back as a resumed conversation or a fresh process from the same
// command depends on whether a `claudeSessionId` was captured for it: with
// one, `claude --resume <id>` (or the engine-native equivalent) picks the
// conversation back up; without one there's no resume point, so the process
// just starts again from its original command.

import { useTerminalStore, type TerminalTab } from '@/terminal/session-store';

export interface RestartSessionRow {
	id: string;
	title: string;
	/** Has a captured engine-native session id — resumes the conversation. */
	resumable: boolean;
	/** Daemon-backed — survives the restart at all. A non-persistent tab
	 *  doesn't carry over and isn't listed here. */
	persistent: boolean;
}

function isLiveForRestart(tab: TerminalTab): boolean {
	return tab.status === 'running' && tab.mode === 'persistent';
}

export function useLiveSessionsForRestart(): RestartSessionRow[] {
	const tabs = useTerminalStore((s) => s.tabs);
	return tabs.filter(isLiveForRestart).map((t) => ({
		id: t.id,
		title: t.title,
		resumable: !!t.claudeSessionId,
		persistent: true,
	}));
}

export function useLiveSessionCount(): number {
	const tabs = useTerminalStore((s) => s.tabs);
	return tabs.filter(isLiveForRestart).length;
}
