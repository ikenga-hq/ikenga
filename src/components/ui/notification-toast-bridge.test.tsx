// honest-failure-states D-20 — `NotificationToastBridge` end to end on the TS
// side: the live-sync callback it registers must queue a toast for `created`
// and stay silent for `updated` (copy changed within one episode).

import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { NotificationRow, NotificationsChangedEvent } from '@/lib/tauri-cmd';

const captured: { onEvent: ((event: NotificationsChangedEvent) => void) | null } = {
	onEvent: null,
};

vi.mock('@/lib/queries/notifications', () => ({
	useNotificationsLiveSync: (onEvent?: (event: NotificationsChangedEvent) => void) => {
		captured.onEvent = onEvent ?? null;
	},
}));

import { NotificationToastBridge } from './floating-toast-chip';

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

function fire(event: NotificationsChangedEvent) {
	act(() => {
		captured.onEvent?.(event);
	});
}

afterEach(() => {
	cleanup();
	captured.onEvent = null;
});

describe('NotificationToastBridge', () => {
	it('an "updated" event refreshes silently: no toast is queued', () => {
		render(<NotificationToastBridge />);
		expect(captured.onEvent).not.toBeNull();

		fire({
			reason: 'updated',
			notification: { ...ROW, title: 'WSL DNS is failing · Ubuntu' },
			muted: false,
		});
		expect(screen.queryByText('WSL DNS is failing · Ubuntu')).toBeNull();
	});

	it('a "created" event shows the row as a toast', () => {
		render(<NotificationToastBridge />);
		fire({ reason: 'created', notification: ROW, muted: false });
		expect(screen.getByText('WSL has no network · Ubuntu')).toBeTruthy();
	});
});
