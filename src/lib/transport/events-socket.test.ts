import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { connectionStateStore } from './connection-state';
import { EventsSocketClient } from './events-socket';

/** Minimal WebSocket stand-in, same shape as `fs-socket.test.ts` uses. */
class FakeSocket {
	onopen: (() => void) | null = null;
	onclose: ((ev?: { code?: number; reason?: string }) => void) | null = null;
	onerror: ((e: unknown) => void) | null = null;
	onmessage: ((e: { data: unknown }) => void) | null = null;
	sent: string[] = [];
	closed = false;

	send(raw: string) {
		this.sent.push(raw);
	}
	close() {
		this.closed = true;
	}
	open() {
		this.onopen?.();
	}
	frame(payload: unknown) {
		this.onmessage?.({ data: JSON.stringify(payload) });
	}
	event(event: string, payload: unknown) {
		this.frame({ type: 'event', event, payload });
	}
	drop(code?: number) {
		this.onclose?.({ code });
	}
	outbound(): Array<{ type: string; events: string[] }> {
		return this.sent.map((s) => JSON.parse(s));
	}
}

function harness() {
	const sockets: FakeSocket[] = [];
	const client = new EventsSocketClient(() => {
		const s = new FakeSocket();
		sockets.push(s);
		return s as unknown as WebSocket;
	});
	return { sockets, client };
}

describe('events-socket client', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		connectionStateStore.__reset();
	});
	afterEach(() => {
		vi.useRealTimers();
		vi.restoreAllMocks();
	});

	it('opens one socket for every listener and subscribes each name once', () => {
		const h = harness();
		h.client.listen('settings://changed', () => {});
		h.client.listen('settings://changed', () => {});
		h.client.listen('projects:active-changed', () => {});
		expect(h.sockets).toHaveLength(1);

		h.sockets[0].open();
		expect(h.sockets[0].outbound()).toEqual([
			{ type: 'subscribe', events: ['settings://changed', 'projects:active-changed'] },
		]);
	});

	it('delivers an event to every listener of that name, in the Tauri shape', () => {
		const h = harness();
		const a = vi.fn();
		const b = vi.fn();
		const other = vi.fn();
		h.client.listen('notifications://changed', a);
		h.client.listen('notifications://changed', b);
		h.client.listen('settings://changed', other);
		h.sockets[0].open();

		const payload = { reason: 'created', notification: { id: 1 }, muted: false };
		h.sockets[0].event('notifications://changed', payload);
		expect(a).toHaveBeenCalledWith({ event: 'notifications://changed', payload });
		expect(b).toHaveBeenCalledTimes(1);
		expect(other).not.toHaveBeenCalled();
	});

	it('the same handler registered twice is two listeners, as with Tauri', () => {
		const h = harness();
		const handler = vi.fn();
		const off1 = h.client.listen('x://y', handler);
		h.client.listen('x://y', handler);
		h.sockets[0].open();
		h.sockets[0].event('x://y', 1);
		expect(handler).toHaveBeenCalledTimes(2);
		off1();
		h.sockets[0].event('x://y', 2);
		expect(handler).toHaveBeenCalledTimes(3);
	});

	it('unlisten stops delivery, unsubscribes the last one, and closes when nothing is left', () => {
		const h = harness();
		const a = vi.fn();
		const b = vi.fn();
		const offA = h.client.listen('settings://changed', a);
		const offB = h.client.listen('settings://changed', b);
		h.sockets[0].open();

		offA();
		// Another listener remains: no unsubscribe yet.
		expect(h.sockets[0].outbound().some((f) => f.type === 'unsubscribe')).toBe(false);
		h.sockets[0].event('settings://changed', { path: '/p' });
		expect(a).not.toHaveBeenCalled();
		expect(b).toHaveBeenCalledTimes(1);

		offB();
		expect(h.sockets[0].outbound().at(-1)).toEqual({
			type: 'unsubscribe',
			events: ['settings://changed'],
		});
		expect(h.sockets[0].closed).toBe(true);
		// Idempotent.
		offB();
		expect(h.sockets[0].outbound().filter((f) => f.type === 'unsubscribe')).toHaveLength(1);
	});

	it('a handler that unlistens during delivery does not break the fan-out', () => {
		const h = harness();
		const late = vi.fn();
		const off = h.client.listen('e', () => off());
		h.client.listen('e', late);
		h.sockets[0].open();
		h.sockets[0].event('e', null);
		expect(late).toHaveBeenCalledTimes(1);
	});

	it('reconnects with backoff, re-subscribes, and keeps every listener', () => {
		const h = harness();
		const handler = vi.fn();
		h.client.listen('projects:active-changed', handler);
		h.sockets[0].open();

		h.sockets[0].drop();
		expect(h.sockets).toHaveLength(1);
		vi.advanceTimersByTime(999);
		expect(h.sockets).toHaveLength(1);
		vi.advanceTimersByTime(1);
		expect(h.sockets).toHaveLength(2);

		// A second failure backs off longer (1s → 2s).
		h.sockets[1].drop();
		vi.advanceTimersByTime(1999);
		expect(h.sockets).toHaveLength(2);
		vi.advanceTimersByTime(1);
		expect(h.sockets).toHaveLength(3);

		h.sockets[2].open();
		expect(h.sockets[2].outbound()).toEqual([
			{ type: 'subscribe', events: ['projects:active-changed'] },
		]);
		h.sockets[2].event('projects:active-changed', { id: 'p9' });
		expect(handler).toHaveBeenCalledWith({
			event: 'projects:active-changed',
			payload: { id: 'p9' },
		});
	});

	it('a listener added while disconnected rides the next connection', () => {
		const h = harness();
		h.client.listen('a', () => {});
		h.sockets[0].open();
		h.sockets[0].drop();
		const late = vi.fn();
		h.client.listen('b', late);
		// Still waiting out the backoff: no extra socket.
		expect(h.sockets).toHaveLength(1);
		vi.advanceTimersByTime(1000);
		h.sockets[1].open();
		expect(h.sockets[1].outbound()).toEqual([{ type: 'subscribe', events: ['a', 'b'] }]);
		h.sockets[1].event('b', 1);
		expect(late).toHaveBeenCalledTimes(1);
	});

	it('hints the notification views to refetch after a reconnect, not on the first open', () => {
		const h = harness();
		const handler = vi.fn();
		h.client.listen('notifications://changed', handler);
		h.sockets[0].open();
		expect(handler).not.toHaveBeenCalled();

		h.sockets[0].drop();
		vi.advanceTimersByTime(1000);
		h.sockets[1].open();
		expect(handler).toHaveBeenCalledWith({
			event: 'notifications://changed',
			payload: { reason: 'read_all', notification: null, muted: false },
		});
	});

	it('stops retrying on 4401 (revoked)', () => {
		const h = harness();
		h.client.listen('a', () => {});
		h.sockets[0].open();
		h.sockets[0].drop(4401);
		vi.advanceTimersByTime(60_000);
		expect(h.sockets).toHaveLength(1);
	});

	it('reconnects at once on 4403 (caps changed)', () => {
		const h = harness();
		h.client.listen('a', () => {});
		h.sockets[0].open();
		h.sockets[0].drop(4403);
		expect(h.sockets).toHaveLength(2);
	});

	it('notes once, after ready, each name the server will never feed', () => {
		const info = vi.spyOn(console, 'info').mockImplementation(() => {});
		const h = harness();
		h.client.listen('settings://changed', () => {});
		h.client.listen('hooks://event', () => {});
		h.client.listen('notifications://changed', () => {});
		// Nothing is said before the server says what it publishes.
		expect(info).not.toHaveBeenCalled();
		h.sockets[0].open();
		h.sockets[0].frame({
			type: 'ready',
			events: ['settings://changed'],
			withheld: ['notifications://changed'],
		});
		expect(info).toHaveBeenCalledTimes(2);
		const lines = info.mock.calls.map((c) => String(c[0]));
		expect(lines.some((l) => l.includes("'hooks://event' has no producer"))).toBe(true);
		expect(lines.some((l) => l.includes("'notifications://changed' is not available"))).toBe(true);
		expect(lines.some((l) => l.includes('settings://changed'))).toBe(false);

		// Again, and on a later listener of the same name: still once.
		h.sockets[0].frame({ type: 'ready', events: ['settings://changed'], withheld: [] });
		h.client.listen('hooks://event', () => {});
		expect(info).toHaveBeenCalledTimes(2);
		// A new dead name, after ready, is noted right away.
		h.client.listen('statusline://snapshot', () => {});
		expect(info).toHaveBeenCalledTimes(3);
	});

	it('marks the connection healthy when the socket opens', () => {
		const h = harness();
		const spy = vi.spyOn(connectionStateStore, 'socketConnected');
		h.client.listen('a', () => {});
		h.sockets[0].open();
		expect(spy).toHaveBeenCalledWith('events');
	});

	describe('ping', () => {
		it('resolves with the round trip once the matching pong arrives', async () => {
			const h = harness();
			h.client.listen('a', () => {});
			const now = vi.spyOn(performance, 'now');
			now.mockReturnValue(1000);
			h.sockets[0].open();
			const p = h.client.ping();
			const sent = h.sockets[0].sent.map((s) => JSON.parse(s)).find((m) => m.type === 'ping');
			expect(sent).toEqual({ type: 'ping', id: expect.any(Number) });
			now.mockReturnValue(1340);
			h.sockets[0].frame({ type: 'pong', id: sent.id });
			await expect(p).resolves.toBe(340);
		});

		it('ignores a pong for another id and times out', async () => {
			const h = harness();
			h.client.listen('a', () => {});
			h.sockets[0].open();
			const p = h.client.ping(1000);
			const caught = p.catch((e) => e);
			h.sockets[0].frame({ type: 'pong', id: 9999 });
			vi.advanceTimersByTime(1001);
			const err = await caught;
			expect(err).toMatchObject({ name: 'PingError', reason: 'timeout' });
		});

		it('never opens a socket: no socket is "not-open"', async () => {
			const h = harness();
			await expect(h.client.ping()).rejects.toMatchObject({ reason: 'not-open' });
			expect(h.sockets).toHaveLength(0);
		});

		it('fails in-flight pings when the socket drops', async () => {
			const h = harness();
			h.client.listen('a', () => {});
			h.sockets[0].open();
			const caught = h.client.ping().catch((e) => e);
			h.sockets[0].drop();
			expect(await caught).toMatchObject({ reason: 'closed' });
		});

		it('treats the old-server error frame as unsupported, until a reconnect', async () => {
			vi.spyOn(console, 'warn').mockImplementation(() => {});
			const h = harness();
			h.client.listen('a', () => {});
			h.sockets[0].open();
			const caught = h.client.ping().catch((e) => e);
			h.sockets[0].frame({
				type: 'error',
				message: 'bad events control frame: unknown variant `ping`',
			});
			expect(await caught).toMatchObject({ reason: 'unsupported' });
			await expect(h.client.ping()).rejects.toMatchObject({ reason: 'unsupported' });
		});
	});
});
