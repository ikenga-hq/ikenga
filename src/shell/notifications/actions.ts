// WP-40b — maps a `NotificationRow.action` (WP-40's producer-authored JSON,
// `{ kind, ...params }`, see `src-tauri/src/notifications/producers.rs` and
// `NotificationAction` in `src/lib/tauri-cmd.ts`) to the button(s) a
// popover row renders. Toasts (`components/ui/floating-toast-chip.tsx`) are
// text-only transient copies and carry no action. The action is narrowed at
// runtime with WP-40's `asKnownNotificationAction` (an ACP
// `permission.decide` becomes open-only `open.thread`), and a hooks-gate
// Allow / Deny is offered only while the ask is live (`isPermissionAskLive`).
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
import { asKnownNotificationAction } from '@/lib/notifications/action-kind';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { NotificationRow } from '@/lib/tauri-cmd';

// Duplicated from `src/shell/status-bar.tsx`'s `NGWA_LINKS` rather than
// imported: this module is reached from the popover, and `status-bar.tsx`
// pulls in the bell that pulls in the popover — importing `NGWA_LINKS` here
// would close that loop into a real import cycle. Two route strings, kept in
// sync by hand.
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

/**
 * How long a held hooks-gate ask can possibly still be answerable, from its
 * row's `createdAt`: the backend parks the `PreToolUse` response for
 * `GATE_HOLD_SECS` (30 s, `src-tauri/src/iyke/hook_settings.rs`) and curl
 * gives up at 35 s. Past this the gate has answered itself (timed out as
 * deny) even if the row never got its `resolvedAt` — e.g. the app quit
 * mid-hold. A little slack for clock skew between the DB write and render.
 */
export const HOOK_GATE_ANSWERABLE_MS = 40_000;

/**
 * Whether a hooks-gate `permission.decide` row can still take Allow / Deny.
 * `resolvedAt` is authoritative (WP-40 sets it when the human decides, the
 * gate times out, or the ask is otherwise over). A row from an older backend
 * without the field falls back to its read state — the gate marks its row
 * read when the ask ends. Either way an ask older than the hold window has
 * expired.
 */
export function isPermissionAskLive(row: NotificationRow, now: number = Date.now()): boolean {
	if (row.resolvedAt != null) return false;
	if (row.resolvedAt === undefined && row.readAt != null) return false;
	return now - row.createdAt < HOOK_GATE_ANSWERABLE_MS;
}

export function notificationActionButtons(
	row: NotificationRow,
	now: number = Date.now(),
): NotificationActionButton[] {
	const action = asKnownNotificationAction(row.action);
	if (!action) {
		return row.kind === 'invite'
			? [{ label: 'Open People', variant: 'ghost', run: () => navigate('/settings/people') }]
			: [];
	}

	switch (action.kind) {
		case 'permission.decide': {
			// Resolved or expired: the decision is over, never offer a dead
			// Allow / Deny. Fall back to opening the terminal that asked.
			if (!isPermissionAskLive(row, now)) {
				const terminalId = action.terminalId;
				return terminalId
					? [{ label: 'Open terminal', variant: 'ghost', run: () => openTerminalPane(terminalId) }]
					: [];
			}
			const { requestId } = action;
			return [
				{ label: 'Allow once', variant: 'primary', run: () => postHookDecision(requestId, 'approved') },
				{ label: 'Deny', variant: 'ghost', run: () => postHookDecision(requestId, 'denied') },
			];
		}
		case 'open.terminal': {
			const sessionId = action.terminalId ?? action.sessionId;
			return sessionId
				? [{ label: 'Open terminal', variant: 'ghost', run: () => openTerminalPane(sessionId) }]
				: [];
		}
		case 'open.thread': {
			// ACP asks are open-only: the thread's own dialog answers them.
			const { threadId } = action;
			return threadId
				? [{ label: 'Open thread', variant: 'ghost', run: () => openTerminalPane(threadId) }]
				: [];
		}
		case 'open.chi_run': {
			const label = action.status === 'failed' ? 'Open log' : 'Open artifact';
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
