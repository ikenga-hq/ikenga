import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { connectionStateStore } from './connection-state';
import { attachRemotePty } from './pty-socket';

/** Minimal WebSocket stand-in the attach loop can drive. */
class FakeSocket {
	onopen: (() => void) | null = null;
	onclose: (() => void) | null = null;
	onerror: ((e: unknown) => void) | null = null;
	onmessage: ((e: { data: unknown }) => void) | null = null;
	closed = false;

	close() {
		this.closed = true;
	}

	open() {
		this.onopen?.();
	}
	text(payload: unknown) {
		this.onmessage?.({ data: JSON.stringify(payload) });
	}
	raw(payload: string) {
		this.onmessage?.({ data: payload });
	}
	binary(s: string) {
		const bytes = new TextEncoder().encode(s);
		// Copy into a standalone ArrayBuffer — `instanceof ArrayBuffer` is the
		// discriminator the attach loop uses.
		const buf = new ArrayBuffer(bytes.length);
		new Uint8Array(buf).set(bytes);
		this.onmessage?.({ data: buf });
	}
	drop() {
		this.onclose?.();
	}
}

function harness() {
	const sockets: FakeSocket[] = [];
	const opens: Array<{ id: string; spawn: boolean }> = [];
	const chunks: Array<{ text: string; endOffset: number }> = [];
	const onExit = vi.fn();

	const open = (id: string, opts?: { spawn?: boolean }) => {
		opens.push({ id, spawn: opts?.spawn ?? false });
		const s = new FakeSocket();
		sockets.push(s);
		return s as unknown as WebSocket;
	};

	const detach = attachRemotePty(
		open,
		'term-1',
		(bytes, endOffset) => chunks.push({ text: new TextDecoder().decode(bytes), endOffset }),
		onExit
	);

	/** Only the daemon's bytes, with banners filtered out. */
	const streamText = () =>
		chunks
			.filter((c) => !c.text.includes('──'))
			.map((c) => c.text)
			.join('');

	return { sockets, opens, chunks, onExit, detach, streamText };
}

describe('attachRemotePty', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		connectionStateStore.__reset();
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it('emits the first connection scrollback verbatim', () => {
		const h = harness();
		h.sockets[0].open();
		h.sockets[0].text({ type: 'ikenga.snapshot', end_offset: 5, len: 5 });
		h.sockets[0].binary('hello');

		expect(h.streamText()).toBe('hello');
		h.detach();
	});

	// The defect this guards: the daemon replays its whole ring on every
	// reconnect, and pty-bridge's only dedup (`dedupUpTo`) is null unless an
	// external snapshot primed it. Emitting the replay verbatim re-paints the
	// entire scrollback into the terminal on every single reconnect.
	it('replays only the bytes produced while the socket was down', () => {
		const h = harness();
		h.sockets[0].open();
		h.sockets[0].text({ type: 'ikenga.snapshot', end_offset: 5, len: 5 });
		h.sockets[0].binary('hello');
		h.sockets[0].drop();

		vi.advanceTimersByTime(1000);
		expect(h.sockets).toHaveLength(2);
		h.sockets[1].open();
		// Ring now holds "hello world" (11 bytes); we already painted 5.
		h.sockets[1].text({ type: 'ikenga.snapshot', end_offset: 11, len: 11 });
		h.sockets[1].binary('hello world');

		expect(h.streamText()).toBe('hello world');
		h.detach();
	});

	it('emits nothing extra when nothing happened during the gap', () => {
		const h = harness();
		h.sockets[0].open();
		h.sockets[0].text({ type: 'ikenga.snapshot', end_offset: 5, len: 5 });
		h.sockets[0].binary('hello');
		h.sockets[0].drop();

		vi.advanceTimersByTime(1000);
		h.sockets[1].open();
		h.sockets[1].text({ type: 'ikenga.snapshot', end_offset: 5, len: 5 });
		h.sockets[1].binary('hello');

		expect(h.streamText()).toBe('hello');
		h.detach();
	});

	it('says so when the ring wrapped past what we had painted', () => {
		const h = harness();
		h.sockets[0].open();
		h.sockets[0].text({ type: 'ikenga.snapshot', end_offset: 5, len: 5 });
		h.sockets[0].binary('hello');
		h.sockets[0].drop();

		vi.advanceTimersByTime(1000);
		h.sockets[1].open();
		// Ring holds bytes 20..30 — everything from 5..20 is gone for good.
		h.sockets[1].text({ type: 'ikenga.snapshot', end_offset: 30, len: 10 });
		h.sockets[1].binary('XXXXXXXXXX');

		expect(h.chunks.some((c) => c.text.includes('15 bytes of output were lost'))).toBe(true);
		h.detach();
	});

	// Banner bytes feed `pty-bridge.streamOffset`, which is reconciled against
	// the capture ring. Counting locally-generated text would skew that for the
	// rest of the session.
	it('does not advance the stream cursor for its own banners', () => {
		const h = harness();
		h.sockets[0].open();
		h.sockets[0].text({ type: 'ikenga.snapshot', end_offset: 5, len: 5 });
		h.sockets[0].binary('hello');
		h.sockets[0].drop();
		vi.advanceTimersByTime(1000);
		h.sockets[1].open();
		h.sockets[1].text({ type: 'ikenga.snapshot', end_offset: 8, len: 8 });
		h.sockets[1].binary('hello!!!');

		// 5 + 3 real bytes, regardless of how many banners were painted.
		expect(Math.max(...h.chunks.map((c) => c.endOffset))).toBe(8);
		h.detach();
	});

	it('only asks the daemon to spawn on the very first attach', () => {
		const h = harness();
		h.sockets[0].open();
		h.sockets[0].drop();
		vi.advanceTimersByTime(1000);

		expect(h.opens[0].spawn).toBe(true);
		// A reconnect that spawns silently replaces a shell the user exited
		// with a brand new one wearing the same id.
		expect(h.opens[1].spawn).toBe(false);
		h.detach();
	});

	it('treats an explicit exit as an exit, not a disconnect', () => {
		const h = harness();
		h.sockets[0].open();
		h.sockets[0].text({ type: 'ikenga.exit', id: 'term-1', code: 0 });
		h.sockets[0].drop();
		vi.advanceTimersByTime(60_000);

		expect(h.onExit).toHaveBeenCalledWith(0);
		expect(h.sockets).toHaveLength(1);
	});

	it('reports the shell exit code when the daemon supplies one', () => {
		const h = harness();
		h.sockets[0].open();
		h.sockets[0].text({ type: 'ikenga.exit', id: 'term-1', code: 130 });
		expect(h.onExit).toHaveBeenCalledWith(130);
		vi.advanceTimersByTime(60_000);
		expect(h.sockets).toHaveLength(1);
	});

	// Attaching to a session that exited during the gap: the daemon paints its
	// retained scrollback, then says the shell is gone.
	it('accepts scrollback followed by exit on an already-exited session', () => {
		const h = harness();
		h.sockets[0].open();
		h.sockets[0].text({ type: 'ikenga.snapshot', end_offset: 6, len: 6 });
		h.sockets[0].binary('bye!\r\n');
		h.sockets[0].text({ type: 'ikenga.exit', id: 'term-1', code: 0 });

		expect(h.streamText()).toBe('bye!\r\n');
		expect(h.onExit).toHaveBeenCalledWith(0);
		vi.advanceTimersByTime(60_000);
		expect(h.sockets).toHaveLength(1);
	});

	it('stops reconnecting when the session is gone', () => {
		const h = harness();
		h.sockets[0].open();
		h.sockets[0].text({ type: 'ikenga.gone', id: 'term-1' });
		vi.advanceTimersByTime(60_000);

		expect(h.onExit).toHaveBeenCalledWith(null);
		expect(h.sockets).toHaveLength(1);
	});

	it('gives up after a bounded number of attempts', () => {
		const h = harness();
		for (let i = 0; i < 20; i++) {
			h.sockets[h.sockets.length - 1].drop();
			vi.advanceTimersByTime(20_000);
		}
		expect(h.sockets.length).toBeLessThanOrEqual(11);
		expect(h.onExit).toHaveBeenCalledWith(null);
	});

	it('paints one disconnect banner, not one per retry', () => {
		const h = harness();
		h.sockets[0].open();
		for (let i = 0; i < 4; i++) {
			h.sockets[h.sockets.length - 1].drop();
			vi.advanceTimersByTime(20_000);
		}
		const banners = h.chunks.filter((c) => c.text.includes('disconnected ·'));
		expect(banners).toHaveLength(1);
		h.detach();
	});

	it('passes unrecognised text frames through as terminal output', () => {
		const h = harness();
		h.sockets[0].open();
		h.sockets[0].raw('Error: something went wrong');
		expect(h.streamText()).toContain('Error: something went wrong');
		h.detach();
	});

	it('registers and clears its terminal in the connection store', () => {
		const h = harness();
		h.sockets[0].open();
		expect(connectionStateStore.get().activeTerminals).toBe(1);

		h.detach();
		expect(connectionStateStore.get().activeTerminals).toBe(0);
		expect(connectionStateStore.get().state).toBe('connected');
	});
});
