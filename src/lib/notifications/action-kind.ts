// WP-40 — runtime narrowing for `NotificationRow.action`. The types live in
// `src/lib/tauri-cmd.ts` (re-exported from there too); this module has no
// runtime imports so it stays testable without Tauri.

import type {
	KnownNotificationAction,
	KnownNotificationActionKind,
	NotificationAction,
} from '@/lib/tauri-cmd';

export const KNOWN_NOTIFICATION_ACTION_KINDS = [
	'permission.decide',
	'open.thread',
	'open.terminal',
	'open.chi_run',
	'open.release_notes',
	'open.pkg_updates',
	'open.violations',
] as const satisfies readonly KnownNotificationActionKind[];

/**
 * The action as a narrowable `KnownNotificationAction`, or `null` when there
 * is none or its kind is unknown to this build. `permission.decide` is
 * hooks-only: one without a `via` is read as `'hooks'`, and a stray
 * `via: 'acp'` (never answerable from here — no engine resolve path yet) is
 * read as the open-only `open.thread` it really is.
 */
export function asKnownNotificationAction(
	action: NotificationAction | null | undefined,
): KnownNotificationAction | null {
	if (!action || typeof action.kind !== 'string') return null;
	if (!(KNOWN_NOTIFICATION_ACTION_KINDS as readonly string[]).includes(action.kind)) return null;
	if (action.kind === 'permission.decide' && action.via === 'acp') {
		return {
			kind: 'open.thread',
			// The typed union no longer carries an ACP permission.decide
			// variant (WP-40 round 2), so read the legacy fields untyped.
			threadId: String((action as Record<string, unknown>).threadId ?? ''),
			requestId: String((action as Record<string, unknown>).requestId ?? ''),
		};
	}
	if (action.kind === 'permission.decide' && action.via !== 'hooks') {
		return { ...action, via: 'hooks' } as unknown as KnownNotificationAction;
	}
	return action as KnownNotificationAction;
}
