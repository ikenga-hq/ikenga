// WP-40 — notifications query surface. Written under DEC-50; not run until
// the 5b close (WP-47).

import { QueryClient } from '@tanstack/react-query';
import { afterEach, describe, expect, it, vi } from 'vitest';

const cmd = vi.hoisted(() => ({
	listen: vi.fn(),
	notificationsList: vi.fn(),
	notificationsUnreadCount: vi.fn(),
	notificationsMuteState: vi.fn(),
	notificationsMarkRead: vi.fn(),
	notificationsMarkAllRead: vi.fn(),
	notificationsMuteKind: vi.fn(),
	notificationsUnmuteKind: vi.fn(),
}));

vi.mock('@/lib/tauri-cmd', () => ({
	...cmd,
	NOTIFICATIONS_CHANGED_EVENT: 'notifications://changed',
}));

import { queryKeys } from '@/lib/query-keys';
import type { NotificationRow } from '@/lib/tauri-cmd';
import {
	asKnownNotificationAction,
	groupNotificationsByDay,
	isNotificationKindMutable,
	isNotificationResolved,
	notificationDecision,
	MUTABLE_NOTIFICATION_KINDS,
	NOTIFICATION_KINDS,
	notificationsListQueryOptions,
	notificationsMuteStateQueryOptions,
	notificationsUnreadCountQueryOptions,
	subscribeNotificationChanges,
} from './notifications';

function row(partial: Partial<NotificationRow>): NotificationRow {
	return {
		id: 1,
		kind: 'update',
		title: 't',
		body: null,
		action: null,
		source: 'test',
		dedupeKey: null,
		count: 1,
		createdAt: 0,
		updatedAt: 0,
		readAt: null,
		...partial,
	};
}

describe('notifications queries', () => {
	afterEach(() => {
		for (const fn of Object.values(cmd)) fn.mockReset();
	});

	it('unread count query calls the command under a stable key', async () => {
		cmd.notificationsUnreadCount.mockResolvedValue({ total: 2, byKind: { permission: 2 } });
		const qc = new QueryClient();
		const data = await qc.fetchQuery(notificationsUnreadCountQueryOptions());
		expect(data).toEqual({ total: 2, byKind: { permission: 2 } });
		expect(cmd.notificationsUnreadCount).toHaveBeenCalledOnce();
		expect(notificationsUnreadCountQueryOptions().queryKey).toEqual(
			queryKeys.notifications.unreadCount(),
		);
	});

	it('list query forwards the filter and keys on it order-insensitively', async () => {
		cmd.notificationsList.mockResolvedValue([]);
		const qc = new QueryClient();
		await qc.fetchQuery(
			notificationsListQueryOptions({ unreadOnly: true, kinds: ['update', 'permission'] }),
		);
		expect(cmd.notificationsList).toHaveBeenCalledWith({
			unreadOnly: true,
			kinds: ['update', 'permission'],
		});
		expect(
			notificationsListQueryOptions({ kinds: ['update', 'permission'] }).queryKey,
		).toEqual(notificationsListQueryOptions({ kinds: ['permission', 'update'] }).queryKey);
		expect(notificationsListQueryOptions().queryKey[0]).toBe('notifications');
		expect(notificationsMuteStateQueryOptions().queryKey).toEqual(
			queryKeys.notifications.muteState(),
		);
	});

	it('permission and violation are the only unmutable kinds', () => {
		expect(NOTIFICATION_KINDS.filter((k) => !isNotificationKindMutable(k))).toEqual([
			'permission',
			'violation',
		]);
		expect([...MUTABLE_NOTIFICATION_KINDS]).toEqual([
			'run_finished',
			'run_failed',
			'update',
			'invite',
		]);
	});

	it('a notifications://changed event invalidates every notifications query', async () => {
		let handler: ((e: { event: string; payload: unknown }) => void) | undefined;
		const unlisten = vi.fn();
		cmd.listen.mockImplementation(async (event: string, cb: typeof handler) => {
			expect(event).toBe('notifications://changed');
			handler = cb;
			return unlisten;
		});
		const qc = new QueryClient();
		const invalidate = vi.spyOn(qc, 'invalidateQueries');
		const seen = vi.fn();
		const stop = await subscribeNotificationChanges(qc, seen);

		const payload = { reason: 'created', notification: row({ id: 9 }), muted: true };
		handler?.({ event: 'notifications://changed', payload });

		expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.notifications.all });
		expect(seen).toHaveBeenCalledWith(payload);
		stop();
		expect(unlisten).toHaveBeenCalledOnce();
	});

	it('groups rows into Today / Earlier on local midnight', () => {
		const now = new Date(2026, 8, 25, 10, 0, 0).getTime();
		const midnight = new Date(2026, 8, 25, 0, 0, 0).getTime();
		const rows = [
			row({ id: 1, updatedAt: now - 60_000 }),
			row({ id: 2, updatedAt: midnight }),
			row({ id: 3, updatedAt: midnight - 1 }),
		];
		const { today, earlier } = groupNotificationsByDay(rows, now);
		expect(today.map((r) => r.id)).toEqual([1, 2]);
		expect(earlier.map((r) => r.id)).toEqual([3]);
	});

	it('resolved rows are dead asks: no inline decision', () => {
		const hooks = row({
			kind: 'permission',
			action: { kind: 'permission.decide', via: 'hooks', requestId: 'r1', terminalId: 't1' },
		});
		expect(isNotificationResolved(hooks)).toBe(false);
		expect(notificationDecision(hooks)?.via).toBe('hooks');
		expect(notificationDecision({ ...hooks, resolvedAt: 5 })).toBeNull();
		expect(isNotificationResolved({ ...hooks, resolvedAt: 5 })).toBe(true);
		// Rows from before `resolvedAt` existed read as open.
		expect(isNotificationResolved({ ...hooks, resolvedAt: undefined })).toBe(false);
	});

	it('ACP asks and terminal prompts are open-only: no inline decision', () => {
		const acp = row({
			kind: 'permission',
			action: { kind: 'open.thread', threadId: 'th', requestId: 'r2' },
		});
		expect(notificationDecision(acp)).toBeNull();
		expect(asKnownNotificationAction(acp.action)?.kind).toBe('open.thread');
		const terminal = row({
			kind: 'permission',
			action: { kind: 'open.terminal', terminalId: 't', sessionId: null },
		});
		expect(notificationDecision(terminal)).toBeNull();
		expect(asKnownNotificationAction(terminal.action)?.kind).toBe('open.terminal');
	});
});
