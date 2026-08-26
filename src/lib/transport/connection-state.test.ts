import { beforeEach, describe, expect, it, vi } from 'vitest';
import { connectionStateStore } from './connection-state';

describe('connectionStateStore', () => {
	beforeEach(() => connectionStateStore.__reset());

	it('starts in connected state by default', () => {
		const current = connectionStateStore.get();
		expect(current.state).toBe('connected');
		expect(current.attempt).toBe(0);
	});

	it('notifies subscribers on state updates', () => {
		const listener = vi.fn();
		const unsubscribe = connectionStateStore.subscribe(listener);

		expect(listener).toHaveBeenCalledWith(
			expect.objectContaining({ state: 'connected', attempt: 0 })
		);

		connectionStateStore.socketDisconnected('t1', 2, 4000);

		expect(listener).toHaveBeenLastCalledWith(
			expect.objectContaining({ state: 'reconnecting', attempt: 2, nextRetryDelayMs: 4000 })
		);

		unsubscribe();
	});

	// The bug this guards: a single shared status let any terminal's teardown
	// write `connected`, so a healthy terminal closing cleared the banner while
	// another was still down.
	it('one socket recovering does not clear another socket that is still down', () => {
		connectionStateStore.socketDisconnected('t1', 3, 8000);
		connectionStateStore.socketDisconnected('t2', 1, 1000);

		connectionStateStore.socketConnected('t2');

		const info = connectionStateStore.get();
		expect(info.state).toBe('reconnecting');
		expect(info.attempt).toBe(3);
	});

	it('reports the worst attempt and the soonest retry across sockets', () => {
		connectionStateStore.socketDisconnected('t1', 5, 16000);
		connectionStateStore.socketDisconnected('t2', 2, 2000);

		const info = connectionStateStore.get();
		expect(info.attempt).toBe(5);
		expect(info.nextRetryDelayMs).toBe(2000);
	});

	it('goes fully disconnected once every down socket has stopped retrying', () => {
		connectionStateStore.socketDisconnected('t1', 11, 0);
		expect(connectionStateStore.get().state).toBe('disconnected');
	});

	it('returns to connected when the last down socket recovers', () => {
		connectionStateStore.socketDisconnected('t1', 1, 1000);
		connectionStateStore.socketConnected('t1');
		expect(connectionStateStore.get().state).toBe('connected');
	});

	it('counts only terminals and agent turns that actually registered', () => {
		expect(connectionStateStore.get().activeTerminals).toBe(0);
		expect(connectionStateStore.get().activeAgentTurns).toBe(0);

		connectionStateStore.terminalOpened('t1');
		connectionStateStore.terminalOpened('t2');
		connectionStateStore.agentTurnStarted('thread-a');
		expect(connectionStateStore.get().activeTerminals).toBe(2);
		expect(connectionStateStore.get().activeAgentTurns).toBe(1);

		connectionStateStore.terminalClosed('t1');
		connectionStateStore.agentTurnEnded('thread-a');
		expect(connectionStateStore.get().activeTerminals).toBe(1);
		expect(connectionStateStore.get().activeAgentTurns).toBe(0);
	});
});
