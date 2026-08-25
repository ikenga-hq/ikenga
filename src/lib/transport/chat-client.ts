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

export class ChatWebSocketClient {
	private ws: WebSocket | null = null;
	private threadId: string;
	private onUpdate: (update: ChatSessionUpdate) => void;

	constructor(threadId: string, onUpdate: (update: ChatSessionUpdate) => void) {
		this.threadId = threadId;
		this.onUpdate = onUpdate;
	}

	connect(): void {
		const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
		const uri = `${protocol}//${window.location.host}/ws/chat/${this.threadId}`;
		this.ws = new WebSocket(uri);

		this.ws.onmessage = (event) => {
			if (typeof event.data === 'string') {
				try {
					const parsed = JSON.parse(event.data);
					this.onUpdate(parsed);
				} catch (e) {
					console.error('Failed to parse chat update:', e);
				}
			}
		};
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
	}

	cancel(): void {
		if (this.ws && this.ws.readyState === WebSocket.OPEN) {
			this.ws.send(JSON.stringify({ type: 'cancel' }));
		}
	}

	disconnect(): void {
		if (this.ws) {
			this.ws.close();
			this.ws = null;
		}
	}
}
