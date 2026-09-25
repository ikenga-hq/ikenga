// WP-40b — maps a `NotificationRow.action` (WP-40's producer-authored JSON,
// `{ kind, ...params }`, see `src-tauri/src/notifications/producers.rs` and
// `NotificationAction` in `src/lib/tauri-cmd.ts`) to the button(s) the
// popover row and the toast bridge both render. One source of truth so the
// two surfaces (`popover.tsx`, `components/ui/floating-toast-chip.tsx`)
// never drift on what "Review" or "Allow once" actually does.
//
// Deep-link fidelity notes (best-effort — no dedicated routes exist yet for
// some targets; see the WP-40b PR body):
//   - `open.terminal` / `open.thread` open a terminal pane keyed by the
//     given session/thread id, the same `addTab({ kind: 'terminal',
//     sessionId })` call the Explorer's Sessions section already uses
//     (`src/shell/explorer/sections/sessions.tsx`) — there is no separate
//     "open a chat thread" pane kind in this tree yet.
//   - `open.chi_run` has no run-detail route (WP-42 owns `/automations`'s
//     real run-history view and hasn't landed); it falls back to
//     `/automations?view=runs`, the same redirect target `/agent-runs`
//     already resolves to.
//   - `open.release_notes` falls back to `/settings/about` (WP-41's
//     release-notes sheet hasn't landed).
//   - `invite` ships no producer yet (D-05's people surface doesn't exist),
//     so its button is a guess: `/settings/people`.

import { iykeFetch } from '@/lib/iyke/client';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { NotificationAction, NotificationRow } from '@/lib/tauri-cmd';

// Duplicated from `src/shell/status-bar.tsx`'s `NGWA_LINKS` rather than
// imported: this module is reached from `components/ui/floating-toast-chip.tsx`
// (the toast bridge), and `status-bar.tsx` pulls in the bell that pulls in
// the toast bridge — importing `NGWA_LINKS` here would close that loop into
// a real import cycle. Two route strings, kept in sync by hand.
const NGWA_UPDATES_ROUTE = '/packages?filter=updates';
const NGWA_VIOLATIONS_ROUTE = '/packages?filter=review';

export interface NotificationActionButton {
	label: string;
	variant: 'primary' | 'ghost';
	run: () => void;
}

function navigate(path: string): void {
	usePaneStore.getState().navigateFocused(path);
}

function openTerminalPane(sessionId: string): void {
	const { focusedId, addTab } = usePaneStore.getState();
	addTab(focusedId, { kind: 'terminal', sessionId });
}

/** Same call `src/terminal/permission-inbox.tsx` makes for the held hooks
 *  gate — best-effort, matching its own `.catch(() => {})`. */
function postHookDecision(requestId: string, decision: 'approved' | 'denied'): void {
	void iykeFetch('/iyke/hooks/decision', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ requestId, decision }),
	}).catch(() => {});
}

export function notificationActionButtons(row: NotificationRow): NotificationActionButton[] {
	const action = row.action;
	if (!action) {
		return row.kind === 'invite'
			? [{ label: 'Open People', variant: 'ghost', run: () => navigate('/settings/people') }]
			: [];
	}

	switch (action.kind) {
		case 'permission.decide': {
			const a = action as Extract<NotificationAction, { kind: 'permission.decide' }>;
			return [
				{ label: 'Allow once', variant: 'primary', run: () => postHookDecision(a.requestId, 'approved') },
				{ label: 'Deny', variant: 'ghost', run: () => postHookDecision(a.requestId, 'denied') },
			];
		}
		case 'open.terminal': {
			const a = action as Extract<NotificationAction, { kind: 'open.terminal' }>;
			const sessionId = a.terminalId ?? a.sessionId;
			return sessionId
				? [{ label: 'Open terminal', variant: 'ghost', run: () => openTerminalPane(sessionId) }]
				: [];
		}
		case 'open.thread': {
			const a = action as Extract<NotificationAction, { kind: 'open.thread' }>;
			return [{ label: 'Open thread', variant: 'ghost', run: () => openTerminalPane(a.threadId) }];
		}
		case 'open.chi_run': {
			const a = action as Extract<NotificationAction, { kind: 'open.chi_run' }>;
			const label = a.status === 'failed' ? 'Open log' : 'Open artifact';
			return [{ label, variant: 'ghost', run: () => navigate('/automations?view=runs') }];
		}
		case 'open.release_notes':
			return [{ label: 'Release notes', variant: 'ghost', run: () => navigate('/settings/about') }];
		case 'open.pkg_updates':
			return [{ label: 'View update', variant: 'ghost', run: () => navigate(NGWA_UPDATES_ROUTE) }];
		case 'open.violations':
			return [{ label: 'Review', variant: 'primary', run: () => navigate(NGWA_VIOLATIONS_ROUTE) }];
		default:
			return row.kind === 'invite'
				? [{ label: 'Open People', variant: 'ghost', run: () => navigate('/settings/people') }]
				: [];
	}
}
