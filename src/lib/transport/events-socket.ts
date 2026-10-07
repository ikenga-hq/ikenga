/**
 * Browser-side client for the daemon's `/ws/events` socket.
 *
 * The desktop receives live updates (`settings://changed`,
 * `notifications://changed`, `projects:active-changed`, …) on Tauri's event
 * bus. A browser session has none, so `WebRemoteTransport.listen` routes
 * here: one socket for the whole page, shared by every listener, carrying
 * the daemon's event bus (`src-tauri/src/server/events.rs`).
 *
 * ## Wire protocol
 *
 * Matched by hand against `src-tauri/src/server/events_ws.rs` — a field
 * renamed on one side must be renamed on the other.
 *
 * Out: `{type:'subscribe', events:[…]}` · `{type:'unsubscribe', events:[…]}`
 * In:  `{type:'ready', events:[…], withheld:[…]}` ·
 *      `{type:'event', event, payload}` · `{type:'error', message}`
 *
 * ## Semantics
 *
 * `listen(name, handler)` resolves to an unlisten function, as Tauri's does.
 * The socket opens on the first listener and closes when the last one goes;
 * the server is told about a name when its first handler arrives and when
 * its last leaves. A dropped socket reconnects with backoff and re-sends
 * every live subscription, so a listener survives any number of drops.
 *
 * Events published while the socket was down are gone (the daemon keeps no
 * backlog). On a reconnect the notification views get the same refetch hint
 * the desktop forwarder sends after it lagged (`reason: 'read_all'`); the
 * other topics have no payload-free form to fake, so they catch up on their
 * next read.
 *
 * ## Names with no producer
 *
 * `ready` lists every name the daemon publishes for this credential. A name
 * in `withheld` exists but this credential may not read it; a name in
 * neither has no daemon producer (`hooks://event`, `statusline://snapshot`,
 * `runtime://bun`, …) and will never fire in browser mode. Each such name is
 * noted once on the console — a dead subscription should not be mistaken for
 * "nothing happened".
 */

import { connectionStateStore } from './connection-state';
import { capsReconnectAllowed, handleAccessClose } from './ws-close';

/** Longest backoff between reconnect attempts. Never gives up: listeners
 *  are mounted for the life of the page, so a socket that stopped retrying
 *  would silently kill every live view. */
const MAX_BACKOFF_MS = 30_000;

export type OpenEventsSocket = () => WebSocket;
export type EventHandler = (event: { event: string; payload: unknown }) => void;

/** The refetch hint a reconnect hands the notification views. */
const NOTIFICATIONS_EVENT = 'notifications://changed';
const NOTIFICATIONS_REFETCH = { reason: 'read_all', notification: null, muted: false };

export class EventsSocketClient {
	private ws: WebSocket | null = null;
	private open = false;
	private attempt = 0;
	private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
	/** Whether this client has been connected before — a later open is a
	 *  reconnect, after which missed events are hinted. */
	private connectedOnce = false;

	private listeners = new Map<string, Set<EventHandler>>();
	/** The last `ready`: names the daemon publishes for this credential. */
	private live: Set<string> | null = null;
	private withheld: Set<string> = new Set();
	private noted = new Set<string>();

	constructor(private readonly openSocket: OpenEventsSocket) {}

	listen(name: string, handler: EventHandler): () => void {
		let set = this.listeners.get(name);
		const first = !set;
		if (!set) {
			set = new Set();
			this.listeners.set(name, set);
		}
		// Wrap so the same function registered twice is two listeners, as
		// with Tauri's `listen`.
		const entry: EventHandler = (e) => handler(e);
		set.add(entry);
		if (first) {
			this.send({ type: 'subscribe', events: [name] });
			this.noteIfDead(name);
		}
		this.connect();

		let done = false;
		return () => {
			if (done) return;
			done = true;
			const current = this.listeners.get(name);
			if (!current) return;
			current.delete(entry);
			if (current.size > 0) return;
			this.listeners.delete(name);
			this.send({ type: 'unsubscribe', events: [name] });
			// Nothing left to listen to: let the socket go rather than
			// holding one open (and reconnecting it) for no one.
			if (this.listeners.size === 0) this.close();
		};
	}

	/** Deliver an event to every listener for `name` (the socket's frames,
	 *  and the reconnect hint). */
	dispatch(name: string, payload: unknown): void {
		const set = this.listeners.get(name);
		if (!set) return;
		// Copy: a handler may unlisten (itself or another) while we iterate.
		for (const handler of [...set]) {
			try {
				handler({ event: name, payload });
			} catch (e) {
				console.error(`[events-socket] '${name}' handler threw:`, e);
			}
		}
	}

	private send(frame: Record<string, unknown>): void {
		// Only while open: subscriptions are (re)sent in full on every open,
		// so nothing needs queueing.
		if (this.ws && this.open) this.ws.send(JSON.stringify(frame));
	}

	private connect(): void {
		if (this.ws || this.reconnectTimer) return;
		let ws: WebSocket;
		try {
			ws = this.openSocket();
		} catch (e) {
			console.warn('[events-socket] could not open socket:', e);
			this.scheduleReconnect();
			return;
		}
		this.ws = ws;

		ws.onopen = () => {
			this.open = true;
			this.attempt = 0;
			connectionStateStore.socketConnected('events');
			const names = [...this.listeners.keys()];
			if (names.length > 0) ws.send(JSON.stringify({ type: 'subscribe', events: names }));
			if (this.connectedOnce) this.dispatch(NOTIFICATIONS_EVENT, NOTIFICATIONS_REFETCH);
			this.connectedOnce = true;
		};

		ws.onmessage = (e) => {
			if (typeof e.data !== 'string') return;
			this.handleFrame(e.data);
		};

		ws.onerror = (err) => {
			console.warn('[events-socket] WebSocket error:', err);
		};

		ws.onclose = (ev?: CloseEvent) => {
			this.open = false;
			this.ws = null;
			// G-ACCESS §3.10: 4401 → the re-auth overlay, and no retry (it
			// would be refused); 4403 → reconnect now with the new caps.
			const access = handleAccessClose(ev?.code, ev?.reason);
			if (access === 'revoked' || this.listeners.size === 0) return;
			if (access === 'caps_changed' && capsReconnectAllowed(this)) {
				this.attempt = 0;
				this.connect();
				return;
			}
			this.scheduleReconnect();
		};
	}

	private handleFrame(raw: string): void {
		let msg: {
			type?: string;
			event?: string;
			payload?: unknown;
			events?: unknown;
			withheld?: unknown;
			message?: string;
		};
		try {
			msg = JSON.parse(raw);
		} catch {
			console.warn('[events-socket] undecodable frame:', raw);
			return;
		}
		switch (msg.type) {
			case 'ready': {
				const names = (v: unknown) =>
					new Set(Array.isArray(v) ? v.filter((n): n is string => typeof n === 'string') : []);
				this.live = names(msg.events);
				this.withheld = names(msg.withheld);
				for (const name of this.listeners.keys()) this.noteIfDead(name);
				break;
			}
			case 'event':
				if (typeof msg.event === 'string') this.dispatch(msg.event, msg.payload);
				break;
			case 'error':
				console.warn(`[events-socket] ${msg.message ?? 'unknown error'}`);
				break;
			default:
				break;
		}
	}

	/** Once per name, say why a subscription will not fire here. Silent
	 *  until the daemon has said what it publishes. */
	private noteIfDead(name: string): void {
		if (!this.live || this.live.has(name) || this.noted.has(name)) return;
		this.noted.add(name);
		console.info(
			this.withheld.has(name)
				? `[events] '${name}' is not available to this device's access level; it will not fire here.`
				: `[events] '${name}' has no producer on this server; it will not fire in browser mode.`
		);
	}

	private scheduleReconnect(): void {
		if (this.reconnectTimer) return;
		this.attempt += 1;
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
		this.attempt = 0;
		this.connectedOnce = false;
		const ws = this.ws;
		this.ws = null;
		this.open = false;
		if (ws) {
			ws.onclose = null;
			ws.close();
		}
	}
}
