import { connectionStateStore } from './connection-state';
import { getAuthToken } from './index';

function tokenQuery(): string {
	const token = getAuthToken();
	return token ? `?token=${encodeURIComponent(token)}` : '';
}

export interface ChatSessionUpdate {
	jsonrpc: '2.0';
	method: 'session/update';
	params: {
		thread_id: string;
		update: {
			session_id?: string;
			type: string;
			delta?: {
				type: string;
				text?: string;
			};
			status?: string;
			stop_reason?: string;
		};
	};
}

/**
 * NOTE: `/ws/chat/:id` now drives the engine registry for real (WP-11b).
 * Only `antigravity-cli` has a headless driver today; any other registered
 * engine answers with an `error` update rather than a fabricated reply.
 */
export type ChatConnectionState = 'disconnected' | 'connecting' | 'connected' | 'reconnecting';

export interface ChatConnectionStatusInfo {
	state: ChatConnectionState;
	attempt: number;
	nextRetryDelayMs: number;
}

/**
 * ChatWebSocketClient — WebSocket transport client for `/ws/chat/:thread_id`.
 * Features automatic reconnect lifecycle with exponential backoff and connection state tracking.
 */
export class ChatWebSocketClient {
	private ws: WebSocket | null = null;
	private threadId: string;
	private onUpdate: (update: ChatSessionUpdate) => void;
	private onStatusChange?: (info: ChatConnectionStatusInfo) => void;

	private state: ChatConnectionState = 'disconnected';
	private attempt = 0;
	private nextRetryDelayMs = 1000;
	private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
	private isExplicitDisconnect = false;

	constructor(
		threadId: string,
		onUpdate: (update: ChatSessionUpdate) => void,
		onStatusChange?: (info: ChatConnectionStatusInfo) => void
	) {
		this.threadId = threadId;
		this.onUpdate = onUpdate;
		this.onStatusChange = onStatusChange;
	}

	public get connectionState(): ChatConnectionState {
		return this.state;
	}

	private setStatus(
		state: ChatConnectionState,
		attempt = this.attempt,
		nextRetryDelayMs = this.nextRetryDelayMs
	): void {
		this.state = state;
		this.attempt = attempt;
		this.nextRetryDelayMs = nextRetryDelayMs;
		this.onStatusChange?.({ state, attempt, nextRetryDelayMs });
	}

	connect(): void {
		if (this.reconnectTimer) {
			clearTimeout(this.reconnectTimer);
			this.reconnectTimer = null;
		}
		this.isExplicitDisconnect = false;
		this.setStatus(this.attempt > 0 ? 'reconnecting' : 'connecting');

		const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
		const uri = `${protocol}//${window.location.host}/ws/chat/${encodeURIComponent(this.threadId)}${tokenQuery()}`;

		try {
			this.ws = new WebSocket(uri);
		} catch (err) {
			console.error('Failed to create Chat WebSocket:', err);
			this.scheduleReconnect();
			return;
		}

		this.ws.onopen = () => {
			this.attempt = 0;
			this.nextRetryDelayMs = 1000;
			this.setStatus('connected');
		};

		this.ws.onmessage = (event) => {
			if (typeof event.data === 'string') {
				try {
					const parsed = JSON.parse(event.data);
					// `status: idle` is the turn's terminal event; drop the
					// in-flight registration so the count reflects reality
					// even when the turn ended in an error or a cancel.
					if (parsed?.params?.update?.status === 'idle') {
						connectionStateStore.agentTurnEnded(this.threadId);
					}
					this.onUpdate(parsed);
				} catch (e) {
					console.error('Failed to parse chat update:', e);
				}
			}
		};

		this.ws.onerror = (err) => {
			console.warn('[chat-client] WebSocket error:', err);
		};

		this.ws.onclose = () => {
			if (!this.isExplicitDisconnect) {
				this.scheduleReconnect();
			} else {
				this.setStatus('disconnected', 0, 1000);
			}
		};
	}

	private scheduleReconnect(): void {
		this.attempt += 1;
		const delay = Math.min(1000 * Math.pow(2, this.attempt - 1), 16000);
		this.setStatus('reconnecting', this.attempt, delay);

		this.reconnectTimer = setTimeout(() => {
			this.connect();
		}, delay);
	}

	sendPrompt(prompt: string, engine = 'antigravity-cli', cwd?: string, model?: string): void {
		if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
			throw new Error('Chat WebSocket is not connected');
		}
		this.ws.send(
			JSON.stringify({
				type: 'prompt',
				prompt,
				engine,
				cwd,
				model,
			})
		);
		// Registered so the connection banner can state how many agent turns
		// are in flight from something it counted, rather than a constant.
		connectionStateStore.agentTurnStarted(this.threadId);
	}

	cancel(): void {
		if (this.ws && this.ws.readyState === WebSocket.OPEN) {
			this.ws.send(JSON.stringify({ type: 'cancel' }));
		}
	}

	disconnect(): void {
		this.isExplicitDisconnect = true;
		connectionStateStore.agentTurnEnded(this.threadId);
		if (this.reconnectTimer) {
			clearTimeout(this.reconnectTimer);
			this.reconnectTimer = null;
		}
		if (this.ws) {
			this.ws.close();
			this.ws = null;
		}
		this.setStatus('disconnected', 0, 1000);
	}
}
