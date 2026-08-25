import { beforeEach, describe, expect, it, vi } from 'vitest';

// Capture the onData callback the Pty class hands to `ptyListen` so we can
// drive bytes from the test. `ptySpawn` returns a synthetic id; `ptyKill` is a
// no-op.
//
// The attach handshake is mocked to mirror the Rust contract exactly:
// `ptyAttachBegin` gates the stream (nothing may be delivered after it until
// `ptyAttachArm`), and `ptyAttachArm` flushes whatever the session emitted
// during the handshake as the first live chunk, starting at the snapshot's end
// offset. `gate` below is the test's stand-in for the Rust-side held buffer.
let deliver: ((bytes: Uint8Array, endOffset: number) => void) | null = null;
let attachBeginResolve:
	| ((v: { data: Uint8Array; endOffset: number; token: number } | null) => void)
	| null = null;
let armed: number[] = [];

vi.mock('../lib/tauri-cmd', () => ({
	ptySpawn: vi.fn(async () => 'fake-id'),
	ptyListen: vi.fn(async (_id: string, onData: (bytes: Uint8Array, endOffset: number) => void) => {
		deliver = onData;
		return () => undefined;
	}),
	ptyAttachBegin: vi.fn(
		() =>
			new Promise((resolve) => {
				attachBeginResolve = resolve;
			})
	),
	ptyAttachArm: vi.fn(async (_id: string, token: number) => {
		armed.push(token);
		return true;
	}),
	ptyKill: vi.fn(async () => undefined),
	ptyResize: vi.fn(async () => undefined),
	ptyWrite: vi.fn(async () => undefined),
}));

import { Pty } from './pty-bridge';

function bytes(s: string): Uint8Array {
	return new TextEncoder().encode(s);
}

/** Drain microtasks + one macrotask so an in-flight `Pty.attach` progresses
 *  past its `await`s (attach-begin, listener registration, arm). */
function flush(): Promise<void> {
	return new Promise((r) => setTimeout(r, 0));
}

// Running byte offset for the spawn-mode tests, which don't care about the
// exact value (dedup never engages) but must still supply a monotonic one.
let total = 0;
function emit(s: string) {
	const b = bytes(s);
	total += b.length;
	deliver!(b, total);
}

beforeEach(() => {
	deliver = null;
	attachBeginResolve = null;
	armed = [];
	total = 0;
});

describe('Pty replay buffer', () => {
	it('replays pre-subscriber bytes to the first subscriber', async () => {
		const pty = await Pty.spawn({ cwd: '/', cmd: ['bash'] });
		emit('hello ');
		emit('world');

		const seen: string[] = [];
		const off = pty.onData((b) => seen.push(new TextDecoder().decode(b)));
		expect(seen.join('')).toBe('hello world');
		off();
	});

	it('keeps buffered bytes for a subscriber that detaches without a live chunk landing (survives an unrendered attach/detach)', async () => {
		const pty = await Pty.spawn({ cwd: '/', cmd: ['bash'] });
		emit('early');

		// First subscriber replays the backlog but detaches WITHOUT any live
		// byte reaching it while attached — mirrors a React StrictMode
		// double-mount or a mount deferred behind `fonts.ready` that never
		// actually rendered anything. The backlog must not be lost to it.
		const first: string[] = [];
		const off1 = pty.onData((b) => first.push(new TextDecoder().decode(b)));
		expect(first.join('')).toBe('early');
		off1();

		// A late subscriber must still see the backlog: the earlier
		// attach/detach did not consume it.
		const late: string[] = [];
		const off2 = pty.onData((b) => late.push(new TextDecoder().decode(b)));
		expect(late.join('')).toBe('early');

		// Only once THIS subscriber has actually received a live chunk does
		// the backlog release.
		emit('more');
		expect(late.join('')).toBe('earlymore');
		off2();

		// A subsequent subscriber does not see the released backlog again.
		const after: string[] = [];
		const off3 = pty.onData((b) => after.push(new TextDecoder().decode(b)));
		expect(after.join('')).toBe('');
		off3();
	});

	it('re-replays the subscribe-yield-resubscribe gap to every subscriber until a live chunk actually reaches one', async () => {
		const pty = await Pty.spawn({ cwd: '/', cmd: ['bash'] });

		// First subscriber sees live bytes.
		const first: string[] = [];
		const off1 = pty.onData((b) => first.push(new TextDecoder().decode(b)));
		emit('A');
		expect(first.join('')).toBe('A');

		// Yield (Studio attach handoff). Bytes during the gap buffer.
		off1();
		emit('B');
		emit('C');

		// Late subscriber replays 'BC' — the buffer is not released merely by
		// being replayed.
		const late: string[] = [];
		const off2 = pty.onData((b) => late.push(new TextDecoder().decode(b)));
		expect(late.join('')).toBe('BC');

		// Detach again WITHOUT a live byte landing while attached: 'BC' must
		// still be there for the next subscriber — this is the exact
		// StrictMode-double-mount case WP-08 fixed (a subscriber that never
		// rendered anything must not have eaten the backlog).
		off2();
		const third: string[] = [];
		const off3 = pty.onData((b) => third.push(new TextDecoder().decode(b)));
		expect(third.join('')).toBe('BC');

		// Now a live chunk actually reaches this attached subscriber — only
		// this releases the backlog.
		emit('D');
		expect(third.join('')).toBe('BCD');
		off3();

		// A subsequent subscriber must not see the released backlog again.
		const fourth: string[] = [];
		const off4 = pty.onData((b) => fourth.push(new TextDecoder().decode(b)));
		expect(fourth.join('')).toBe('');
		off4();
	});

	it('passes live bytes straight through while a subscriber is attached', async () => {
		const pty = await Pty.spawn({ cwd: '/', cmd: ['bash'] });
		const seen: string[] = [];
		const off = pty.onData((b) => seen.push(new TextDecoder().decode(b)));
		emit('x');
		emit('y');
		expect(seen.join('')).toBe('xy');
		off();
	});
});

describe('Pty detached-attach scrollback replay (atomic handshake)', () => {
	/** Run the attach handshake the way the real one runs: begin resolves with a
	 *  snapshot, THEN the listener registers, THEN arm releases the gate. The
	 *  `gate` callback is invoked at the point Rust would flush its held bytes —
	 *  i.e. the first thing the freshly-registered listener ever receives. */
	async function attachWith(
		snap: { data: Uint8Array; endOffset: number; token: number } | null
	): Promise<Pty> {
		const p = Pty.attach('fake-id', 'x');
		await flush(); // parks on ptyAttachBegin
		attachBeginResolve!(snap);
		return p;
	}

	it('replays the snapshot, then the bytes the gate held, in stream order', async () => {
		const pty = await attachWith({ data: bytes('hello '), endOffset: 6, token: 7 });
		expect(armed).toEqual([7]);

		// Rust flushes what it held during the handshake: offsets 6..11.
		deliver!(bytes('world'), 11);

		const seen: string[] = [];
		const off = pty.onData((b) => seen.push(new TextDecoder().decode(b)));
		expect(seen.join('')).toBe('hello world');
		off();
	});

	it('passes contiguous post-snapshot bytes through without offset arithmetic', async () => {
		// The seam is closed in Rust, so the frontend must NOT second-guess
		// offsets. This asserts the contiguous case (snapshot ends at 6, chunk
		// spans 6..11) reaches the subscriber untouched — i.e. no residual
		// trimming survives on the attach path.
		//
		// NOTE: this does NOT exercise an overlapping chunk. Under the new
		// contract an overlap cannot occur on this path by construction, so
		// there is no in-band way to assert the frontend would pass one
		// through; the Rust seam test (`attach_seam_delivers_the_stream_exactly_once`)
		// is what proves the no-overlap invariant.
		const pty = await attachWith({ data: bytes('hello '), endOffset: 6, token: 1 });
		const seen: string[] = [];
		const off = pty.onData((b) => seen.push(new TextDecoder().decode(b)));
		expect(seen.join('')).toBe('hello ');

		deliver!(bytes('world'), 11);
		deliver!(bytes('!'), 12);
		expect(seen.join('')).toBe('hello world!');
		off();
	});

	it('keeps the snapshot in front of bytes that land before the subscriber mounts', async () => {
		// The gate flush arrives while no xterm is attached yet — it buffers in
		// `replayBuffer` behind the snapshot, and replays in order.
		const pty = await attachWith({ data: bytes('hello '), endOffset: 6, token: 2 });
		deliver!(bytes('wor'), 9);
		deliver!(bytes('ld'), 11);

		const seen: string[] = [];
		const off = pty.onData((b) => seen.push(new TextDecoder().decode(b)));
		expect(seen.join('')).toBe('hello world');
		off();
	});

	it('tolerates a missing snapshot (exited PTY) and still streams live bytes', async () => {
		const pty = await attachWith(null);
		expect(armed).toEqual([]); // nothing to release

		const seen: string[] = [];
		const off = pty.onData((b) => seen.push(new TextDecoder().decode(b)));
		deliver!(bytes('live'), 4);
		expect(seen.join('')).toBe('live');
		off();
	});

	it('updates cwd via setCwd', async () => {
		const pty = await Pty.spawn({ cmd: ['bash'], cwd: '/initial/dir' });
		expect(pty.cwd).toBe('/initial/dir');
		pty.setCwd('/new/updated/dir');
		expect(pty.cwd).toBe('/new/updated/dir');
	});
});
