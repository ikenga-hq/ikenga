// G-ACCESS §3.10 (WP-78a): the remote client's handling of the server's
// deliberate WebSocket closes — 4401 → re-auth, 4403 → access changed.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const showReauth = vi.fn();
vi.mock('./reauth-store', () => ({
	useReauthStore: { getState: () => ({ showReauth }) },
}));
vi.mock('./index', () => ({
	transportToken: () => null,
	withShareQuery: (u: string) => u,
}));

import { ChatWebSocketClient } from './chat-client';
import { connectionStateStore } from './connection-state';
import { getFsSocketClient, resetFsSocketClient } from './fs-socket';
import {
	accessCloseKind,
	CAPS_RECONNECT_WINDOW_MS,
	capsReconnectAllowed,
	handleAccessClose,
} from './ws-close';

class FakeSocket {
	static all: FakeSocket[] = [];
	static OPEN = 1;
	readyState = 0;
	onopen: (() => void) | null = null;
	onclose: ((e?: { code: number; reason: string }) => void) | null = null;
	onerror: ((e: unknown) => void) | null = null;
	onmessage: ((e: { data: unknown }) => void) | null = null;
	sent: string[] = [];
	constructor(public url = '') {
		FakeSocket.all.push(this);
	}
	send(raw: string) {
		this.sent.push(raw);
	}
	close() {}
	open() {
		this.readyState = 1;
		this.onopen?.();
	}
	closeWith(code: number, reason = '') {
		this.onclose?.({ code, reason });
	}
}

beforeEach(() => {
	vi.useFakeTimers();
	showReauth.mockClear();
	FakeSocket.all = [];
	connectionStateStore.__reset();
	resetFsSocketClient();
});
afterEach(() => {
	vi.useRealTimers();
	resetFsSocketClient();
	vi.unstubAllGlobals();
});

describe('ws-close', () => {
	it('classifies only the two access codes', () => {
		expect(accessCloseKind(4401)).toBe('revoked');
		expect(accessCloseKind(4403)).toBe('caps_changed');
		for (const code of [undefined, 1000, 1006, 4400, 4404]) {
			expect(accessCloseKind(code)).toBeNull();
		}
	});

	it('4401 opens the re-auth overlay; 4403 marks access changed; others nothing', async () => {
		expect(handleAccessClose(1006)).toBeNull();
		expect(connectionStateStore.get().access).toBeNull();
		expect(handleAccessClose(4403, 'caps_changed')).toBe('caps_changed');
		expect(connectionStateStore.get().access).toEqual({
			kind: 'caps_changed',
			reason: 'caps_changed',
		});
		expect(showReauth).not.toHaveBeenCalled();
		expect(handleAccessClose(4401, 'epoch_changed')).toBe('revoked');
		await vi.waitFor(() => expect(showReauth).toHaveBeenCalledTimes(1));
		connectionStateStore.socketConnected('x');
		expect(connectionStateStore.get().access).toBeNull();
	});

	it('allows one immediate 4403 reconnect per socket per window', () => {
		const a = {};
		const b = {};
		expect(capsReconnectAllowed(a, 1_000)).toBe(true);
		expect(capsReconnectAllowed(a, 1_000 + CAPS_RECONNECT_WINDOW_MS - 1)).toBe(false);
		expect(capsReconnectAllowed(b, 1_001)).toBe(true);
		expect(capsReconnectAllowed(a, 1_000 + CAPS_RECONNECT_WINDOW_MS)).toBe(true);
	});
});

describe('the fs socket on an access close', () => {
	const open = () => new FakeSocket() as unknown as WebSocket;

	it('4401: re-auth, no reconnect', async () => {
		const client = getFsSocketClient(open);
		void client.watch('/p').catch(() => {});
		FakeSocket.all[0].open();
		FakeSocket.all[0].closeWith(4401, 'device_revoked');
		await vi.advanceTimersByTimeAsync(60_000);
		expect(FakeSocket.all).toHaveLength(1);
		expect(showReauth).toHaveBeenCalledTimes(1);
	});

	it('4403: reconnects at once and re-watches', async () => {
		const client = getFsSocketClient(open);
		void client.watch('/p').catch(() => {});
		FakeSocket.all[0].open();
		FakeSocket.all[0].closeWith(4403, 'caps_changed');
		expect(FakeSocket.all).toHaveLength(2);
		FakeSocket.all[1].open();
		expect(connectionStateStore.get().access).toBeNull();
	});
});

describe('the chat socket on an access close', () => {
	beforeEach(() => {
		vi.stubGlobal('WebSocket', FakeSocket);
	});

	it('4401: disconnected, re-auth, no reconnect', async () => {
		const states: string[] = [];
		const chat = new ChatWebSocketClient(
			't1',
			() => {},
			(i) => states.push(i.state)
		);
		chat.connect();
		FakeSocket.all[0].open();
		FakeSocket.all[0].closeWith(4401, 'device_revoked');
		await vi.advanceTimersByTimeAsync(60_000);
		expect(FakeSocket.all).toHaveLength(1);
		expect(chat.connectionState).toBe('disconnected');
		expect(showReauth).toHaveBeenCalledTimes(1);
	});

	it('4403: reconnects at once', async () => {
		const chat = new ChatWebSocketClient('t2', () => {});
		chat.connect();
		FakeSocket.all[0].open();
		FakeSocket.all[0].closeWith(4403, 'caps_changed');
		expect(FakeSocket.all).toHaveLength(2);
		FakeSocket.all[1].open();
		expect(chat.connectionState).toBe('connected');
		chat.disconnect();
	});
});
