export type RemoteConnectionState = 'connected' | 'reconnecting' | 'disconnected';

export interface RemoteConnectionInfo {
	state: RemoteConnectionState;
	/** Highest retry attempt across all currently-disconnected sockets. */
	attempt: number;
	/** Soonest scheduled retry, in ms, across those sockets. */
	nextRetryDelayMs: number;
	/** Terminals whose PTY socket is currently down. */
	activeTerminals: number;
	/** Agent turns in flight when the connection dropped. */
	activeAgentTurns: number;
}

type ConnectionListener = (info: RemoteConnectionInfo) => void;

interface SocketEntry {
	attempt: number;
	nextRetryDelayMs: number;
}

/**
 * Aggregate remote-connection state.
 *
 * Deliberately keyed by socket id rather than holding a single global status:
 * every terminal drives its own reconnect loop, so a shared scalar lets a
 * healthy terminal's teardown overwrite a sick one's `reconnecting` and the
 * banner then claims everything is fine while a pane is still down.
 *
 * The counts are derived from what is actually registered here. Nothing
 * invents a plausible-looking number — a banner that states "2 terminals are
 * still running" has to be able to name them.
 */
const disconnected = new Map<string, SocketEntry>();
const liveTerminals = new Set<string>();
const liveAgentTurns = new Set<string>();
const listeners = new Set<ConnectionListener>();

let currentInfo: RemoteConnectionInfo = {
	state: 'connected',
	attempt: 0,
	nextRetryDelayMs: 0,
	activeTerminals: 0,
	activeAgentTurns: 0,
};

function recompute(): RemoteConnectionInfo {
	if (disconnected.size === 0) {
		return {
			state: 'connected',
			attempt: 0,
			nextRetryDelayMs: 0,
			activeTerminals: liveTerminals.size,
			activeAgentTurns: liveAgentTurns.size,
		};
	}
	let attempt = 0;
	let soonest = Number.POSITIVE_INFINITY;
	for (const entry of disconnected.values()) {
		if (entry.attempt > attempt) attempt = entry.attempt;
		if (entry.nextRetryDelayMs < soonest) soonest = entry.nextRetryDelayMs;
	}
	return {
		// A socket that has given up retrying reports `nextRetryDelayMs: 0`;
		// when every down socket has, the connection is not coming back on its
		// own and the banner should stop promising a countdown.
		state: soonest > 0 ? 'reconnecting' : 'disconnected',
		attempt,
		nextRetryDelayMs: Number.isFinite(soonest) ? soonest : 0,
		activeTerminals: liveTerminals.size,
		activeAgentTurns: liveAgentTurns.size,
	};
}

function publish() {
	currentInfo = recompute();
	for (const listener of listeners) {
		try {
			listener(currentInfo);
		} catch (e) {
			console.error('[connection-state] listener threw:', e);
		}
	}
}

export const connectionStateStore = {
	get(): RemoteConnectionInfo {
		return currentInfo;
	},

	/** Register a live terminal socket so the banner can count it. */
	terminalOpened(id: string) {
		liveTerminals.add(id);
		publish();
	},
	terminalClosed(id: string) {
		liveTerminals.delete(id);
		disconnected.delete(id);
		publish();
	},

	/** Register an in-flight agent turn so the banner can count it. */
	agentTurnStarted(id: string) {
		liveAgentTurns.add(id);
		publish();
	},
	agentTurnEnded(id: string) {
		liveAgentTurns.delete(id);
		publish();
	},

	/** This socket lost its connection and will retry in `nextRetryDelayMs`. */
	socketDisconnected(id: string, attempt: number, nextRetryDelayMs: number) {
		disconnected.set(id, { attempt, nextRetryDelayMs });
		publish();
	},
	/** This socket is back. Clears only its own entry. */
	socketConnected(id: string) {
		if (disconnected.delete(id)) publish();
	},

	subscribe(listener: ConnectionListener): () => void {
		listeners.add(listener);
		listener(currentInfo);
		return () => {
			listeners.delete(listener);
		};
	},

	/** Test seam. */
	__reset() {
		disconnected.clear();
		liveTerminals.clear();
		liveAgentTurns.clear();
		publish();
	},
};
