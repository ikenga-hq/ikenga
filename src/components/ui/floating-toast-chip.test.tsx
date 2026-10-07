// Auto-dismiss timer must survive parent re-renders (callers pass an inline
// `onDismiss`), and NotificationToastBridge must keep draining its queue.

import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { NotificationRow, NotificationsChangedEvent } from '@/lib/tauri-cmd';

const live = vi.hoisted(() => ({
	handler: null as null | ((e: NotificationsChangedEvent) => void),
}));
vi.mock('@/lib/queries/notifications', () => ({
	useNotificationsLiveSync: (cb: (e: NotificationsChangedEvent) => void) => {
		live.handler = cb;
	},
}));

import { FloatingToastChip, NotificationToastBridge } from './floating-toast-chip';

beforeEach(() => {
	vi.useFakeTimers();
});
afterEach(() => {
	cleanup();
	vi.useRealTimers();
	live.handler = null;
});

describe('<FloatingToastChip /> — auto-dismiss timer', () => {
	it('is not restarted by parent re-renders with a fresh inline onDismiss', () => {
		const dismissed = vi.fn();
		function Parent({ n }: { n: number }) {
			// A new arrow every render, exactly like real callers.
			return <FloatingToastChip label={`hi ${n}`} ttlMs={1000} onDismiss={() => dismissed(n)} />;
		}
		const { rerender } = render(<Parent n={0} />);
		for (let i = 1; i <= 5; i++) {
			act(() => {
				vi.advanceTimersByTime(150);
			});
			rerender(<Parent n={i} />);
		}
		// 750ms elapsed: not yet.
		expect(dismissed).not.toHaveBeenCalled();
		act(() => {
			vi.advanceTimersByTime(250);
		});
		// Fired once, at ttlMs from mount, with the LATEST handler.
		expect(dismissed).toHaveBeenCalledTimes(1);
		expect(dismissed).toHaveBeenCalledWith(5);
	});

	it('does nothing without ttlMs or onDismiss', () => {
		const dismissed = vi.fn();
		render(<FloatingToastChip label="x" onDismiss={dismissed} />);
		act(() => {
			vi.advanceTimersByTime(60_000);
		});
		expect(dismissed).not.toHaveBeenCalled();
	});
});

function row(id: number, title: string): NotificationRow {
	return {
		id,
		kind: 'update',
		title,
		body: null,
		action: null,
		source: 'test',
		dedupeKey: null,
		count: 1,
		createdAt: 1,
		updatedAt: 1,
		readAt: null,
	};
}

function fire(r: NotificationRow, extra: Partial<NotificationsChangedEvent> = {}) {
	act(() => {
		live.handler?.({ reason: 'created', notification: r, muted: false, ...extra });
	});
}

describe('<NotificationToastBridge />', () => {
	it('shows one toast at a time and drains the queue at the 3.2s cadence', () => {
		render(<NotificationToastBridge />);
		fire(row(1, 'First'));
		fire(row(2, 'Second'));
		expect(screen.getByText('First')).toBeTruthy();
		expect(screen.queryByText('Second')).toBeNull();
		act(() => {
			vi.advanceTimersByTime(3200);
		});
		expect(screen.queryByText('First')).toBeNull();
		expect(screen.getByText('Second')).toBeTruthy();
		act(() => {
			vi.advanceTimersByTime(3200);
		});
		expect(screen.queryByText('Second')).toBeNull();
	});

	it('stays quiet for muted events', () => {
		render(<NotificationToastBridge />);
		fire(row(1, 'Muted'), { muted: true });
		expect(screen.queryByText('Muted')).toBeNull();
	});
});
