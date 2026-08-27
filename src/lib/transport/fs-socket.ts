/**
 * Browser-side client for the daemon's `/ws/fs` watcher socket.
 *
 * The desktop app receives watcher events on Tauri's event bus as
 * `fs://{watcherId}`. A browser session has no event bus, so `fsWatch` /
 * `fsListenWatch` / `fsUnwatch` in `tauri-cmd.ts` route here instead.
 *
 * ## Wire protocol
 *
 * Matched by hand against `src-tauri/src/server/fs_ws.rs` — a field renamed on
 * one side must be renamed on the other, and a mismatch reads as `undefined`
 * rather than failing loudly.
 *
 * Out: `{type:'watch', reqId, path}` · `{type:'unwatch', watcherId}`
 * In:  `{type:'fs_ready'}` · `{type:'watched', reqId, watcherId, path}`
 *      `{type:'error', reqId, message}` · `{type:'change', watcherId, kind, path}`
 *
 * ## Why callers get a handle id, not the server's watcher id
 *
 * The socket can drop and come back. On reconnect the daemon has forgotten
 * every watch (its manager is per-connection and dies with the socket), so we
 * re-issue them — and each one comes back with a *new* server id. Callers hold
 * the id `fsWatch` gave them across that, so the id they hold is ours; the
 * server's id lives in the handle and is swapped underneath them.
 */

import type { FileChange } from '../tauri-cmd';

/** Longest backoff between reconnect attempts. */
const MAX_BACKOFF_MS = 16_000;
/** Give up after this many consecutive failures, as `pty-socket` does. */
const MAX_ATTEMPTS = 10;
/** A `watch` request that never gets an answer settles as a rejection. */
const WATCH_TIMEOUT_MS = 15_000;

export type OpenFsSocket = () => WebSocket;

interface Handle {
	/** The path this handle asked for, replayed verbatim on reconnect. */
	path: string;
	/** The daemon's current id for it, or null while unattached. */
	serverId: string | null;
	handler: ((change: FileChange) => void) | null;
}

interface Pending {
	resolve: (handleId: string) => void;
	reject: (err: Error) => void;
	handleId: string;
	timer: ReturnType<typeof setTimeout>;
}

let nextLocalId = 1;

class FsSocketClient {
	private ws: WebSocket | null = null;
	private open = false;
	private attempt = 0;
	private reconnectTimer: ReturnType<typeof setTimeout> | null = null;

	/** Handle id → handle. The id callers hold. */
	private handles = new Map<string, Handle>();
	/** Server watcher id → handle id, for routing `change` frames. */
	private byServerId = new Map<string, string>();
	/** In-flight `watch` requests, keyed by reqId. */
	private pending = new Map<string, Pending>();
	/** Frames queued while the socket is not open yet. */
	private outbox: string[] = [];

	constructor(private readonly openSocket: OpenFsSocket) {}

	watch(path: string): Promise<string> {
		const handleId = `fsw-${nextLocalId++}`;
		this.handles.set(handleId, { path, serverId: null, handler: null });

		return new Promise<string>((resolve, reject) => {
			const reqId = `req-${handleId}`;
			const timer = setTimeout(() => {
				this.pending.delete(reqId);
				this.handles.delete(handleId);
				reject(new Error(`fs watch timed out for ${path}`));
			}, WATCH_TIMEOUT_MS);
			this.pending.set(reqId, { resolve, reject, handleId, timer });
			this.send({ type: 'watch', reqId, path });
			this.connect();
		});
	}

	listen(handleId: string, handler: (change: FileChange) => void): () => void {
		const handle = this.handles.get(handleId);
		// A handler registered against an unwatched (or already-torn-down)
		// handle would never fire; say so rather than pretending.
		if (!handle) {
			console.warn(`[fs-socket] listen() for unknown watcher ${handleId}`);
			return () => {};
		}
		handle.handler = handler;
		return () => {
			const current = this.handles.get(handleId);
			if (current?.handler === handler) current.handler = null;
		};
	}

	unwatch(handleId: string): void {
		const handle = this.handles.get(handleId);
		if (!handle) return;
		this.handles.delete(handleId);
		if (handle.serverId) {
			this.byServerId.delete(handle.serverId);
			this.send({ type: 'unwatch', watcherId: handle.serverId });
		}
		// Nothing left to watch: let the socket go rather than holding one
		// open (and reconnecting it) for a page with no active watchers.
		if (this.handles.size === 0) this.close();
	}

	private send(frame: Record<string, unknown>): void {
		const raw = JSON.stringify(frame);
		if (this.ws && this.open) {
			this.ws.send(raw);
			return;
		}
		this.outbox.push(raw);
	}

	private connect(): void {
		if (this.ws) return;
		let ws: WebSocket;
		try {
			ws = this.openSocket();
		} catch (e) {
			console.warn('[fs-socket] could not open socket:', e);
			this.scheduleReconnect();
			return;
		}
		this.ws = ws;

		ws.onopen = () => {
			this.open = true;
			this.attempt = 0;
			// Re-issue every live watch. On a first connect this is empty and
			// the outbox already holds the initial `watch` frame.
			for (const [handleId, handle] of this.handles) {
				if (handle.serverId !== null) continue;
				if (this.hasPendingFor(handleId)) continue;
				const reqId = `req-${handleId}-r${this.attempt}-${Date.now()}`;
				this.pending.set(reqId, {
					handleId,
					// A re-watch has no caller waiting on it — the original
					// promise already settled. Failures are logged, not thrown.
					resolve: () => {},
					reject: (err) => console.warn(`[fs-socket] re-watch failed: ${err.message}`),
					timer: setTimeout(() => this.pending.delete(reqId), WATCH_TIMEOUT_MS),
				});
				this.outbox.push(JSON.stringify({ type: 'watch', reqId, path: handle.path }));
			}
			const queued = this.outbox;
			this.outbox = [];
			for (const raw of queued) ws.send(raw);
		};

		ws.onmessage = (e) => {
			if (typeof e.data !== 'string') return;
			this.handleFrame(e.data);
		};

		ws.onerror = (err) => {
			console.warn('[fs-socket] WebSocket error:', err);
		};

		ws.onclose = () => {
			this.open = false;
			this.ws = null;
			// Every server id died with the connection.
			this.byServerId.clear();
			for (const handle of this.handles.values()) handle.serverId = null;
			if (this.handles.size === 0) return;
			this.scheduleReconnect();
		};
	}

	private hasPendingFor(handleId: string): boolean {
		for (const p of this.pending.values()) if (p.handleId === handleId) return true;
		return false;
	}

	private handleFrame(raw: string): void {
		let msg: {
			type?: string;
			reqId?: string | null;
			watcherId?: string;
			path?: string;
			kind?: FileChange['kind'];
			message?: string;
		};
		try {
			msg = JSON.parse(raw);
		} catch {
			console.warn('[fs-socket] undecodable frame:', raw);
			return;
		}

		switch (msg.type) {
			case 'fs_ready':
				break;
			case 'watched': {
				const pending = msg.reqId ? this.pending.get(msg.reqId) : undefined;
				if (!pending || !msg.watcherId) return;
				this.pending.delete(msg.reqId as string);
				clearTimeout(pending.timer);
				const handle = this.handles.get(pending.handleId);
				if (!handle) {
					// Unwatched while the request was in flight — don't leave
					// a watcher running on the daemon with nobody reading it.
					this.send({ type: 'unwatch', watcherId: msg.watcherId });
					return;
				}
				handle.serverId = msg.watcherId;
				this.byServerId.set(msg.watcherId, pending.handleId);
				pending.resolve(pending.handleId);
				break;
			}
			case 'error': {
				const pending = msg.reqId ? this.pending.get(msg.reqId) : undefined;
				if (!pending) {
					console.warn(`[fs-socket] ${msg.message ?? 'unknown error'}`);
					return;
				}
				this.pending.delete(msg.reqId as string);
				clearTimeout(pending.timer);
				this.handles.delete(pending.handleId);
				pending.reject(new Error(msg.message ?? 'fs watch failed'));
				break;
			}
			case 'change': {
				if (!msg.watcherId || !msg.path || !msg.kind) return;
				const handleId = this.byServerId.get(msg.watcherId);
				if (!handleId) return;
				const handler = this.handles.get(handleId)?.handler;
				if (handler) handler({ kind: msg.kind, path: msg.path });
				break;
			}
			default:
				break;
		}
	}

	private scheduleReconnect(): void {
		if (this.reconnectTimer) return;
		this.attempt += 1;
		if (this.attempt > MAX_ATTEMPTS) {
			console.warn(
				`[fs-socket] gave up after ${MAX_ATTEMPTS} attempts; file watching is off ` +
					'for this session until something re-watches.'
			);
			// Drop the handles: leaving them would make a later reconnect
			// silently resurrect watchers whose callers are long gone.
			for (const pending of this.pending.values()) clearTimeout(pending.timer);
			this.pending.clear();
			this.handles.clear();
			this.byServerId.clear();
			this.outbox = [];
			this.attempt = 0;
			return;
		}
		const delay = Math.min(1000 * 2 ** (this.attempt - 1), MAX_BACKOFF_MS);
		this.reconnectTimer = setTimeout(() => {
			this.reconnectTimer = null;
			this.connect();
		}, delay);
	}

	private close(): void {
		if (this.reconnectTimer) {
			clearTimeout(this.reconnectTimer);
			this.reconnectTimer = null;
		}
		for (const pending of this.pending.values()) clearTimeout(pending.timer);
		this.pending.clear();
		this.byServerId.clear();
		this.outbox = [];
		this.attempt = 0;
		const ws = this.ws;
		this.ws = null;
		this.open = false;
		if (ws) {
			ws.onclose = null;
			ws.close();
		}
	}
}

let client: FsSocketClient | null = null;

/**
 * One shared client per page. Watchers are cheap on the wire and every
 * consumer wants the same socket; opening one per `fsWatch` would give the
 * daemon a connection per viewer pane.
 */
export function getFsSocketClient(openSocket: OpenFsSocket): {
	watch: (path: string) => Promise<string>;
	listen: (handleId: string, handler: (change: FileChange) => void) => () => void;
	unwatch: (handleId: string) => void;
} {
	if (!client) client = new FsSocketClient(openSocket);
	return client;
}

/** Test seam — drops the shared client so the next call builds a fresh one. */
export function resetFsSocketClient(): void {
	client = null;
}
