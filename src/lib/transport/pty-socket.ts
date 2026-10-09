import { connectionStateStore } from './connection-state';
import { capsReconnectAllowed, handleAccessClose } from './ws-close';

/** Longest backoff between reconnect attempts. */
const MAX_BACKOFF_MS = 16_000;
/**
 * Give up after this many consecutive failures.
 *
 * Retrying forever paints a "disconnected" banner line into the terminal on
 * every attempt, so a permanently-dead daemon grows the scrollback without
 * bound while telling the user, every 16 seconds, that it is about to succeed.
 */
const MAX_ATTEMPTS = 10;

export type OpenPtySocket = (id: string, opts?: { spawn?: boolean }) => WebSocket;

export function createDaemonPtySocketOpener(wsUrl: string, token?: string): OpenPtySocket {
	return (id: string, opts?: { spawn?: boolean }) => {
		const params = new URLSearchParams();
		if (token) params.set('token', token);
		if (opts?.spawn) params.set('spawn', 'true');
		const query = params.toString();
		const base = wsUrl.replace(/^http/, 'ws').replace(/\/+$/, '');
		const socketUrl = `${base}/ws/pty/${encodeURIComponent(id)}${query ? `?${query}` : ''}`;
		const ws = new WebSocket(socketUrl);
		ws.binaryType = 'arraybuffer';
		return ws;
	};
}

/**
 * Open PTY sockets that can carry input, by PTY id.
 *
 * The daemon writes every binary frame a client sends on `/ws/pty/:id` to
 * the PTY, under the same `dispatch` check as the `pty_write` RPC. Typing
 * through the socket keeps keystrokes in order — one stream — where one HTTP
 * request per key does not: on a jittery link two requests on different
 * connections overtake each other and the shell receives `teh` for `the`.
 */
const inputSinks = new Map<string, Set<(bytes: Uint8Array) => boolean>>();

/**
 * Send `data` to PTY `id` over an open socket. False when none is open (not
 * attached yet, or reconnecting); the caller then falls back to the RPC.
 */
export function sendPtyInput(id: string, data: string): boolean {
	const sinks = inputSinks.get(id);
	if (!sinks) return false;
	const bytes = new TextEncoder().encode(data);
	for (const send of sinks) if (send(bytes)) return true;
	return false;
}

interface SnapshotControl {
	type: 'ikenga.snapshot';
	end_offset: number;
	len: number;
}

const YELLOW = '\x1b[38;2;234;179;8m';
const GREEN = '\x1b[38;2;34;197;94m';
const RESET = '\x1b[0m';

function banner(color: string, text: string): Uint8Array {
	return new TextEncoder().encode(
		`\r\n${color}── ${text} ${'─'.repeat(Math.max(0, 48 - text.length))}${RESET}\r\n`
	);
}

/**
 * Attach to a remote PTY over the daemon's WebSocket, with reconnect.
 *
 * ## Why this tracks two different offsets
 *
 * `onData`'s `endOffset` is **connection-relative**: `pty-bridge` uses it only
 * to derive each chunk's start, and the first frame of a connection is where
 * that connection's stream begins. The daemon's `end_offset` is **absolute**
 * into the session's ring.
 *
 * On reconnect the daemon replays its whole scrollback ring. Emitting that
 * verbatim re-paints the entire buffer into the terminal — `pty-bridge` only
 * dedups against `dedupUpTo`, which is null unless an external snapshot primed
 * it, so nothing downstream would catch it. Tracking the absolute cursor lets
 * us emit only the genuinely-new tail of the replay: no duplication, and no
 * loss of whatever the shell printed while the socket was down.
 */
export function attachRemotePty(
	openPtySocket: OpenPtySocket,
	id: string,
	onData: (bytes: Uint8Array, endOffset: number) => void,
	onExit: (code: number | null) => void,
	onSessionLost?: () => void
): () => void {
	let ws: WebSocket | null = null;
	let closedByCaller = false;
	let attempt = 0;
	let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
	let everConnected = false;
	/** This attachment's identity for the 4403 immediate-reconnect guard. */
	const capsKey = {};

	/** Bytes handed to `onData` so far — connection-relative, monotonic. */
	let received = 0;
	/** Absolute end of the daemon stream we have already painted. */
	let serverOffset: number | null = null;
	/** The `ikenga.snapshot` header awaiting its binary payload. */
	let pendingSnapshot: SnapshotControl | null = null;

	const emit = (bytes: Uint8Array) => {
		if (bytes.length === 0) return;
		received += bytes.length;
		onData(bytes, received);
	};

	/**
	 * Local UI text (reconnect banners) must NOT advance the stream cursor —
	 * `received` feeds `pty-bridge.streamOffset`, which is reconciled against
	 * the capture ring. Bytes the daemon never sent would skew that for the
	 * life of the session.
	 */
	const emitLocal = (bytes: Uint8Array) => {
		onData(bytes, received);
	};

	const finish = (code: number | null) => {
		closedByCaller = true;
		if (reconnectTimer) clearTimeout(reconnectTimer);
		connectionStateStore.terminalClosed(id);
		onExit(code);
	};

	const connect = () => {
		if (closedByCaller) return;
		// Only the very first attach may create the session. A reconnect that
		// spawns would silently replace a shell the user exited with a brand
		// new one wearing the same id.
		ws = openPtySocket(id, { spawn: !everConnected });

		ws.onopen = () => {
			everConnected = true;
			attempt = 0;
			connectionStateStore.socketConnected(id);
			connectionStateStore.terminalOpened(id);
		};

		ws.onmessage = (e) => {
			if (typeof e.data === 'string') {
				handleControlFrame(e.data);
				return;
			}
			if (!(e.data instanceof ArrayBuffer)) return;
			const bytes = new Uint8Array(e.data);

			if (pendingSnapshot) {
				const snap = pendingSnapshot;
				pendingSnapshot = null;
				emitSnapshot(snap, bytes);
				return;
			}

			if (serverOffset !== null) serverOffset += bytes.length;
			emit(bytes);
		};

		ws.onerror = (err) => {
			console.warn(`[pty-socket] WebSocket error for ${id}:`, err);
		};

		ws.onclose = (ev?: CloseEvent) => {
			if (closedByCaller) return;
			// G-ACCESS §3.10. The shell keeps running on the host either way.
			const access = handleAccessClose(ev?.code, ev?.reason);
			if (access === 'revoked') {
				// The credential is dead: a retry would be refused. The re-auth
				// overlay is up; signing in again reloads into a fresh attach.
				closedByCaller = true;
				if (reconnectTimer) clearTimeout(reconnectTimer);
				connectionStateStore.terminalClosed(id);
				emitLocal(banner(YELLOW, 'signed out · sign in again to reattach'));
				return;
			}
			if (access === 'caps_changed' && capsReconnectAllowed(capsKey)) {
				// "The client reconnects at once with its new caps."
				attempt = 0;
				connectionStateStore.terminalClosed(id);
				reconnectTimer = setTimeout(connect, 0);
				return;
			}
			scheduleReconnect();
		};
	};

	const handleControlFrame = (raw: string) => {
		let msg: {
			type?: string;
			end_offset?: number;
			len?: number;
			message?: string;
			code?: number;
		} | null = null;
		try {
			msg = JSON.parse(raw);
		} catch {
			msg = null;
		}
		// Not a control frame — the daemon still sends bare text for a few
		// error paths, and those belong in the terminal.
		if (!msg || typeof msg.type !== 'string' || !msg.type.startsWith('ikenga.')) {
			emit(new TextEncoder().encode(raw));
			return;
		}

		switch (msg.type) {
			case 'ikenga.snapshot':
				pendingSnapshot = {
					type: 'ikenga.snapshot',
					end_offset: msg.end_offset ?? 0,
					len: msg.len ?? 0,
				};
				// A zero-length snapshot has no binary frame following it, so
				// settle the cursor now.
				if (pendingSnapshot.len === 0) {
					if (serverOffset === null) serverOffset = pendingSnapshot.end_offset;
					pendingSnapshot = null;
				}
				break;
			case 'ikenga.exit':
				// The shell exited. `code` is the real exit status when the
				// daemon knows it; attaching to an already-exited session
				// delivers its scrollback first and then lands here.
				finish(typeof msg.code === 'number' ? msg.code : null);
				break;
			case 'ikenga.gone':
				// The session no longer exists at all — retention elapsed and
				// it was reaped. Reconnecting cannot bring it back, and asking
				// the daemon to spawn one would fabricate a replacement.
				emitLocal(banner(YELLOW, 'terminal session no longer exists on the host'));
				onSessionLost?.();
				finish(null);
				break;
			case 'ikenga.error':
				// The daemon could not attach at all. Surface it and stop —
				// retrying an attach the daemon just refused only repeats it.
				emitLocal(banner(YELLOW, `attach failed · ${msg.message ?? 'unknown error'}`));
				onSessionLost?.();
				finish(null);
				break;
			default:
				break;
		}
	};

	const emitSnapshot = (snap: SnapshotControl, bytes: Uint8Array) => {
		const snapshotStart = snap.end_offset - bytes.length;

		if (serverOffset === null) {
			// First connection: the replay IS this connection's stream start.
			serverOffset = snap.end_offset;
			emit(bytes);
			return;
		}

		// Reconnect. Everything up to `serverOffset` is already on screen.
		if (snap.end_offset <= serverOffset) {
			// Nothing new happened while we were away.
			emitLocal(banner(GREEN, 'reattached'));
			return;
		}

		if (snapshotStart > serverOffset) {
			// The ring wrapped past our cursor: output was produced during the
			// gap that the daemon no longer holds. Say so rather than
			// presenting a silently discontinuous buffer.
			const lost = snapshotStart - serverOffset;
			emitLocal(banner(YELLOW, `reattached · ${lost} bytes of output were lost`));
			serverOffset = snap.end_offset;
			emit(bytes);
			return;
		}

		const fresh = bytes.subarray(serverOffset - snapshotStart);
		serverOffset = snap.end_offset;
		emitLocal(banner(GREEN, 'reattached'));
		emit(fresh);
	};

	const scheduleReconnect = () => {
		connectionStateStore.terminalClosed(id);
		attempt += 1;

		if (attempt > MAX_ATTEMPTS) {
			emitLocal(banner(YELLOW, `disconnected · gave up after ${MAX_ATTEMPTS} attempts`));
			connectionStateStore.socketDisconnected(id, attempt, 0);
			onSessionLost?.();
			finish(null);
			return;
		}

		const delay = Math.min(1000 * 2 ** (attempt - 1), MAX_BACKOFF_MS);
		connectionStateStore.socketDisconnected(id, attempt, delay);

		// One banner only, on the first drop. Repeating it every retry is how
		// the scrollback fills up while nothing is happening.
		if (attempt === 1) {
			const timeStr = new Date().toLocaleTimeString('en-GB', { hour12: false });
			emitLocal(banner(YELLOW, `disconnected · ${timeStr}`));
		}

		reconnectTimer = setTimeout(connect, delay);
	};

	// Binary, never text: the daemon tries a text frame as a JSON control
	// message first, and typed text must not be able to spell one.
	const sink = (bytes: Uint8Array): boolean => {
		if (closedByCaller || !ws || ws.readyState !== WebSocket.OPEN) return false;
		ws.send(bytes);
		return true;
	};
	let sinks = inputSinks.get(id);
	if (!sinks) {
		sinks = new Set();
		inputSinks.set(id, sinks);
	}
	sinks.add(sink);

	connect();

	return () => {
		closedByCaller = true;
		if (reconnectTimer) clearTimeout(reconnectTimer);
		connectionStateStore.terminalClosed(id);
		const own = inputSinks.get(id);
		own?.delete(sink);
		if (own && own.size === 0) inputSinks.delete(id);
		if (ws) ws.close();
	};
}
