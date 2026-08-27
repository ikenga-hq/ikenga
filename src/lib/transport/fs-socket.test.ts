import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { FileChange } from '../tauri-cmd';
import { getFsSocketClient, resetFsSocketClient } from './fs-socket';

/** Minimal WebSocket stand-in, same shape as `pty-socket.test.ts` uses. */
class FakeSocket {
	onopen: (() => void) | null = null;
	onclose: (() => void) | null = null;
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
	/** A server → client frame. */
	frame(payload: unknown) {
		this.onmessage?.({ data: JSON.stringify(payload) });
	}
	drop() {
		this.onclose?.();
	}
	/** Everything this socket was asked to send, decoded. */
	outbound(): Array<Record<string, string>> {
		return this.sent.map((s) => JSON.parse(s));
	}
}

function harness() {
	const sockets: FakeSocket[] = [];
	const open = () => {
		const s = new FakeSocket();
		sockets.push(s);
		return s as unknown as WebSocket;
	};
	return { sockets, client: getFsSocketClient(open) };
}

/** Answer the newest `watch` frame on `s` with a `watched` carrying `serverId`. */
function grantWatch(s: FakeSocket, serverId: string) {
	const req = s
		.outbound()
		.filter((f) => f.type === 'watch')
		.at(-1);
	if (!req) throw new Error('no watch frame to grant');
	s.frame({ type: 'watched', reqId: req.reqId, watcherId: serverId, path: req.path });
}

describe('fs-socket client', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		resetFsSocketClient();
	});
	afterEach(() => {
		vi.useRealTimers();
		resetFsSocketClient();
	});

	it('queues the watch frame until the socket opens, then flushes it', async () => {
		const h = harness();
		const p = h.client.watch('/tmp/a');
		// Nothing can be on the wire yet — `watch()` is called before `onopen`.
		expect(h.sockets[0].sent).toEqual([]);

		h.sockets[0].open();
		const sentWatch = h.sockets[0].outbound().find((f) => f.type === 'watch');
		expect(sentWatch?.path).toBe('/tmp/a');

		grantWatch(h.sockets[0], 'srv-1');
		await expect(p).resolves.toMatch(/^fsw-/);
	});

	it('routes change frames to the handler registered for that server id', async () => {
		const h = harness();
		const p = h.client.watch('/tmp/a');
		h.sockets[0].open();
		grantWatch(h.sockets[0], 'srv-1');
		const handleId = await p;

		const seen: FileChange[] = [];
		h.client.listen(handleId, (c) => seen.push(c));

		h.sockets[0].frame({
			type: 'change',
			watcherId: 'srv-1',
			kind: 'modify',
			path: '/tmp/a/x.txt',
		});
		// A change for a watcher this page does not hold must not fan out.
		h.sockets[0].frame({
			type: 'change',
			watcherId: 'srv-999',
			kind: 'modify',
			path: '/tmp/elsewhere',
		});

		expect(seen).toEqual([{ kind: 'modify', path: '/tmp/a/x.txt' }]);
	});

	/**
	 * The whole reason callers get a handle id instead of the daemon's:
	 * a reconnect re-issues the watch and the daemon answers with a NEW id,
	 * while the caller keeps holding the one `watch()` returned.
	 */
	it('keeps the caller handle stable across a reconnect and re-routes the new server id', async () => {
		const h = harness();
		const p = h.client.watch('/tmp/a');
		h.sockets[0].open();
		grantWatch(h.sockets[0], 'srv-1');
		const handleId = await p;

		const seen: FileChange[] = [];
		h.client.listen(handleId, (c) => seen.push(c));

		h.sockets[0].drop();
		await vi.advanceTimersByTimeAsync(1500);
		expect(h.sockets).toHaveLength(2);

		h.sockets[1].open();
		// The re-watch must ask for the same path, unprompted by the caller.
		const rewatch = h.sockets[1].outbound().find((f) => f.type === 'watch');
		expect(rewatch?.path).toBe('/tmp/a');

		// …and the daemon hands back a DIFFERENT id for it.
		grantWatch(h.sockets[1], 'srv-2');
		h.sockets[1].frame({
			type: 'change',
			watcherId: 'srv-2',
			kind: 'create',
			path: '/tmp/a/new.txt',
		});
		expect(seen).toEqual([{ kind: 'create', path: '/tmp/a/new.txt' }]);

		// The dead id must not still route anywhere.
		h.sockets[1].frame({ type: 'change', watcherId: 'srv-1', kind: 'modify', path: '/tmp/a/o' });
		expect(seen).toHaveLength(1);
	});

	it('rejects the caller when the daemon refuses the path', async () => {
		const h = harness();
		const p = h.client.watch('/etc');
		h.sockets[0].open();
		const req = h.sockets[0].outbound().find((f) => f.type === 'watch');
		h.sockets[0].frame({
			type: 'error',
			reqId: req?.reqId,
			message: 'path outside allowlist: /etc',
		});
		await expect(p).rejects.toThrow(/outside allowlist/);
	});

	it('unwatch sends the daemon its own id, not the caller handle', async () => {
		const h = harness();
		const p = h.client.watch('/tmp/a');
		h.sockets[0].open();
		grantWatch(h.sockets[0], 'srv-1');
		const handleId = await p;

		h.client.unwatch(handleId);
		const bye = h.sockets[0].outbound().find((f) => f.type === 'unwatch');
		expect(bye?.watcherId).toBe('srv-1');
		expect(bye?.watcherId).not.toBe(handleId);
		// Last handle gone → the socket goes too, and stays gone.
		expect(h.sockets[0].closed).toBe(true);
		await vi.advanceTimersByTimeAsync(30_000);
		expect(h.sockets).toHaveLength(1);
	});

	/**
	 * Documents a KNOWN GAP rather than a guarantee.
	 *
	 * `handleFrame`'s `watched` case has a branch that unwatches a grant whose
	 * handle is gone, so the daemon is not left holding a watcher nobody
	 * reads. That branch is unreachable: every site that deletes a handle
	 * (`fs-socket.ts` lines 80, 107, 231, 260) deletes its `pending` entry in
	 * the same breath, and the `watched` case bails on a missing `pending`
	 * first. So a grant that arrives after the 15 s timeout is dropped
	 * silently and the daemon-side watcher lives until the socket closes.
	 *
	 * Bounded, not harmless: `MAX_WATCHERS_PER_CONNECTION` is 64, so enough
	 * timed-out watches on one long-lived socket would start refusing real
	 * ones. Reaching it needs a daemon unresponsive for 15 s at a time.
	 * Flip this test's expectations when the branch is made reachable.
	 */
	it('drops a grant that lands after the timeout, orphaning it on the daemon', async () => {
		const h = harness();
		const p = h.client.watch('/tmp/a');
		h.sockets[0].open();
		const req = h.sockets[0].outbound().find((f) => f.type === 'watch');

		// Attach the rejection handler BEFORE the timer fires — advancing
		// fake timers rejects synchronously, and an unattached rejection
		// surfaces as an unhandled error that fails the file, not the assert.
		const settled = p.then(
			() => null,
			(e: Error) => e
		);
		await vi.advanceTimersByTimeAsync(20_000);
		expect((await settled)?.message).toMatch(/timed out/);

		h.sockets[0].frame({
			type: 'watched',
			reqId: req?.reqId,
			watcherId: 'srv-late',
			path: '/tmp/a',
		});
		expect(h.sockets[0].outbound().find((f) => f.type === 'unwatch')).toBeUndefined();
	});

	it('survives an undecodable frame rather than tearing the socket down', async () => {
		const h = harness();
		const p = h.client.watch('/tmp/a');
		h.sockets[0].open();
		h.sockets[0].onmessage?.({ data: 'not json' });
		grantWatch(h.sockets[0], 'srv-1');
		await expect(p).resolves.toMatch(/^fsw-/);
	});
});
