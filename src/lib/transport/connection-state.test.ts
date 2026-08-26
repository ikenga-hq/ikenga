import { describe, expect, it, vi } from 'vitest';
import { connectionStateStore } from './connection-state';

describe('connectionStateStore', () => {
	it('starts in connected state by default', () => {
		const current = connectionStateStore.get();
		expect(current.state).toBe('connected');
		expect(current.attempt).toBe(0);
	});

	it('notifies subscribers on state updates', () => {
		const listener = vi.fn();
		const unsubscribe = connectionStateStore.subscribe(listener);

		expect(listener).toHaveBeenCalledWith(
			expect.objectContaining({
				state: 'connected',
				attempt: 0,
			})
		);

		connectionStateStore.set({
			state: 'reconnecting',
			attempt: 2,
			nextRetryDelayMs: 4000,
		});

		expect(listener).toHaveBeenLastCalledWith(
			expect.objectContaining({
				state: 'reconnecting',
				attempt: 2,
				nextRetryDelayMs: 4000,
			})
		);

		unsubscribe();
	});
});
