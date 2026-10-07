// honest-failure-states D-20 — which `notifications://changed` events pop a
// toast. A WSL health row whose copy changes within one episode is published
// as `updated`; that must refresh lists without a second toast.

import { describe, expect, it } from 'vitest';
import type { NotificationRow, NotificationsChangedEvent } from '@/lib/tauri-cmd';
import { isToastWorthy } from './floating-toast-chip';

const ROW: NotificationRow = {
	id: 1,
	kind: 'system',
	title: 'WSL has no network · Ubuntu',
	body: null,
	action: { kind: 'fix.wsl_network', distro: 'Ubuntu', state: 'no_route' },
	source: 'wsl.health',
	dedupeKey: 'wsl:network:ubuntu',
	count: 1,
	createdAt: 1,
	updatedAt: 1,
	readAt: null,
	resolvedAt: null,
};

function ev(over: Partial<NotificationsChangedEvent>): NotificationsChangedEvent {
	return { reason: 'created', notification: ROW, muted: false, ...over };
}

describe('isToastWorthy', () => {
	it('toasts a new row and a repeat folded into an unread row', () => {
		expect(isToastWorthy(ev({ reason: 'created' }))).toBe(true);
		expect(isToastWorthy(ev({ reason: 'coalesced' }))).toBe(true);
	});

	it('does not toast an in-place update within an episode (D-20)', () => {
		expect(isToastWorthy(ev({ reason: 'updated', notification: { ...ROW, count: 2 } }))).toBe(
			false
		);
	});

	it('does not toast muted kinds, read-state changes or row-less events', () => {
		expect(isToastWorthy(ev({ muted: true }))).toBe(false);
		expect(isToastWorthy(ev({ reason: 'read', notification: null }))).toBe(false);
		expect(isToastWorthy(ev({ reason: 'mute_changed', notification: null }))).toBe(false);
		expect(isToastWorthy(ev({ notification: null }))).toBe(false);
	});
});
