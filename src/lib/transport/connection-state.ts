export type RemoteConnectionState = 'connected' | 'reconnecting' | 'disconnected';

export interface RemoteConnectionInfo {
	state: RemoteConnectionState;
	attempt: number;
	nextRetryDelayMs: number;
	activeTerminals: number;
	activeAgentTurns: number;
}

type ConnectionListener = (info: RemoteConnectionInfo) => void;

let currentInfo: RemoteConnectionInfo = {
	state: 'connected',
	attempt: 0,
	nextRetryDelayMs: 1000,
	activeTerminals: 0,
	activeAgentTurns: 0,
};

const listeners = new Set<ConnectionListener>();

export const connectionStateStore = {
	get(): RemoteConnectionInfo {
		return currentInfo;
	},
	set(update: Partial<RemoteConnectionInfo>) {
		currentInfo = { ...currentInfo, ...update };
		for (const listener of listeners) {
			try {
				listener(currentInfo);
			} catch (e) {
				console.error('[connection-state] listener threw:', e);
			}
		}
	},
	subscribe(listener: ConnectionListener): () => void {
		listeners.add(listener);
		listener(currentInfo);
		return () => {
			listeners.delete(listener);
		};
	},
};
