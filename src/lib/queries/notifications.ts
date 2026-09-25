// WP-40 — React Query surface for the notifications aggregation table.
//
// Consumers: the notification centre + bell (WP-40b), the daily address on
// the Project dashboard (WP-39), and the toast bridge (WP-40b — toasts become
// transient copies of table rows).
//
// Live updates: Rust publishes every change on `notifications://changed`.
// Mount `useNotificationsLiveSync()` ONCE near the shell root (WP-40b owns
// that mount — this WP ships no UI) so every query below refreshes on change.
// Until something mounts it the queries still work; they just refresh on
// their staleTime / on mutation instead of instantly.
//
// Mute prefs live in settings.json (`workspace.notifications.mutedKinds`),
// read and written in Rust. The list / unread-count commands already exclude
// muted kinds; the mute-state query is only for rendering the mute menu.

import {
	type QueryClient,
	queryOptions,
	useMutation,
	useQueryClient,
} from '@tanstack/react-query';
import { useEffect, useRef } from 'react';
import { asKnownNotificationAction } from '@/lib/notifications/action-kind';
import { queryKeys } from '@/lib/query-keys';
import {
	type KnownNotificationAction,
	listen,
	NOTIFICATIONS_CHANGED_EVENT,
	type NotificationKind,
	type NotificationRow,
	type NotificationsChangedEvent,
	type NotificationsListOptions,
	type NotificationsMuteState,
	notificationsList,
	notificationsMarkAllRead,
	notificationsMarkRead,
	notificationsMuteKind,
	notificationsMuteState,
	notificationsUnmuteKind,
	notificationsUnreadCount,
	type UnlistenFn,
} from '@/lib/tauri-cmd';

export {
	asKnownNotificationAction,
	KNOWN_NOTIFICATION_ACTION_KINDS,
} from '@/lib/notifications/action-kind';

export type {
	KnownNotificationAction,
	KnownNotificationActionKind,
	NotificationAction,
	NotificationKind,
	NotificationRow,
	NotificationsChangedEvent,
	NotificationsMuteState,
	NotificationsUnreadCount,
} from '@/lib/tauri-cmd';

/** Every kind, in the Rust enum's order. */
export const NOTIFICATION_KINDS = [
	'permission',
	'run_finished',
	'run_failed',
	'update',
	'violation',
	'invite',
] as const satisfies readonly NotificationKind[];

/** D-07: "Permission and violation cannot be muted; every other kind can." */
export const MUTABLE_NOTIFICATION_KINDS = [
	'run_finished',
	'run_failed',
	'update',
	'invite',
] as const satisfies readonly NotificationKind[];

export function isNotificationKindMutable(kind: NotificationKind): boolean {
	return (MUTABLE_NOTIFICATION_KINDS as readonly NotificationKind[]).includes(kind);
}

// ─── Queries ────────────────────────────────────────────────────────────────

export type NotificationsListFilter = Omit<NotificationsListOptions, 'before'>;

export function notificationsListQueryOptions(filter: NotificationsListFilter = {}) {
	return queryOptions({
		queryKey: queryKeys.notifications.list(filter),
		queryFn: () => notificationsList(filter),
		staleTime: 15_000,
	});
}

/** Unread total + per-kind, muted kinds excluded. Bell badge + daily address. */
export function notificationsUnreadCountQueryOptions() {
	return queryOptions({
		queryKey: queryKeys.notifications.unreadCount(),
		queryFn: () => notificationsUnreadCount(),
		staleTime: 15_000,
	});
}

export function notificationsMuteStateQueryOptions() {
	return queryOptions({
		queryKey: queryKeys.notifications.muteState(),
		queryFn: () => notificationsMuteState(),
		staleTime: 60_000,
	});
}

// ─── Mutations ──────────────────────────────────────────────────────────────

export function invalidateNotifications(qc: QueryClient): Promise<void> {
	return qc.invalidateQueries({ queryKey: queryKeys.notifications.all });
}

export function useMarkNotificationsRead() {
	const qc = useQueryClient();
	return useMutation<number, Error, number[]>({
		mutationFn: (ids) => notificationsMarkRead(ids),
		onSuccess: () => invalidateNotifications(qc),
	});
}

/** "Mark all read" — pass a kind to limit it, `null` for everything. */
export function useMarkAllNotificationsRead() {
	const qc = useQueryClient();
	return useMutation<number, Error, NotificationKind | null>({
		mutationFn: (kind) => notificationsMarkAllRead(kind),
		onSuccess: () => invalidateNotifications(qc),
	});
}

/**
 * Mute / unmute one kind. Rejects `permission` / `violation` before the call
 * (Rust refuses them too). Un-muting marks read the kind's rows recorded
 * while it was muted, so the badge does not jump by a backlog; rows that were
 * already unread before the mute stay unread.
 */
export function useSetNotificationKindMuted() {
	const qc = useQueryClient();
	return useMutation<
		NotificationsMuteState,
		Error,
		{ kind: NotificationKind; muted: boolean }
	>({
		mutationFn: async ({ kind, muted }) => {
			if (muted && !isNotificationKindMutable(kind)) {
				throw new Error(`${kind} notifications cannot be muted`);
			}
			return muted ? notificationsMuteKind(kind) : notificationsUnmuteKind(kind);
		},
		onSuccess: (state) => {
			qc.setQueryData(queryKeys.notifications.muteState(), state);
			return invalidateNotifications(qc);
		},
	});
}

// ─── Live sync ──────────────────────────────────────────────────────────────

/**
 * Subscribe to `notifications://changed` and invalidate every notifications
 * query on each event. `onEvent` sees the raw event (the toast bridge uses it:
 * `reason === 'created' && !muted` → show a transient copy of the row).
 */
export function subscribeNotificationChanges(
	qc: QueryClient,
	onEvent?: (event: NotificationsChangedEvent) => void,
): Promise<UnlistenFn> {
	return listen<NotificationsChangedEvent>(NOTIFICATIONS_CHANGED_EVENT, (event) => {
		void invalidateNotifications(qc);
		onEvent?.(event.payload);
	});
}

/** Mount once near the shell root. See the module header. */
export function useNotificationsLiveSync(
	onEvent?: (event: NotificationsChangedEvent) => void,
): void {
	const qc = useQueryClient();
	const onEventRef = useRef(onEvent);
	onEventRef.current = onEvent;
	useEffect(() => {
		let unlisten: UnlistenFn | null = null;
		let active = true;
		void subscribeNotificationChanges(qc, (e) => onEventRef.current?.(e)).then((stop) => {
			if (active) unlisten = stop;
			else stop();
		});
		return () => {
			active = false;
			unlisten?.();
		};
	}, [qc]);
}

// ─── View helpers ───────────────────────────────────────────────────────────

/** The thing the row asks about is over (see `NotificationRow.resolvedAt`). */
export function isNotificationResolved(row: NotificationRow): boolean {
	return row.resolvedAt != null;
}

/**
 * The row's inline Allow / Deny, or `null` when it has none to offer: not a
 * `permission.decide` action, or already resolved (a dead ask). Branch on
 * `via` to answer: `'hooks'` → `/iyke/hooks/decision`, `'acp'` → the chat
 * engine's permission-respond path for `threadId`.
 */
export function notificationDecision(
	row: NotificationRow,
): Extract<KnownNotificationAction, { kind: 'permission.decide' }> | null {
	if (isNotificationResolved(row)) return null;
	const action = asKnownNotificationAction(row.action);
	return action?.kind === 'permission.decide' ? action : null;
}

/** Local midnight of `now`. */
function startOfDay(now: number): number {
	const d = new Date(now);
	d.setHours(0, 0, 0, 0);
	return d.getTime();
}

/**
 * D-07 popover grouping: rows whose latest occurrence is today (local time)
 * vs earlier. Input order is preserved inside each group.
 */
export function groupNotificationsByDay(
	rows: readonly NotificationRow[],
	now: number = Date.now(),
): { today: NotificationRow[]; earlier: NotificationRow[] } {
	const cutoff = startOfDay(now);
	const today: NotificationRow[] = [];
	const earlier: NotificationRow[] = [];
	for (const row of rows) {
		(row.updatedAt >= cutoff ? today : earlier).push(row);
	}
	return { today, earlier };
}
