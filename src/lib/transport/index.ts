export interface RpcTransport {
	invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T>;
	listen<T>(event: string, handler: (event: { event: string; payload: T }) => void): Promise<() => void>;
	openPtySocket?(id: string): WebSocket;
}

export function isTauri(): boolean {
	if (typeof window === 'undefined') return false;
	return '__TAURI_INTERNALS__' in window || '__TAURI__' in window;
}

export function getAuthToken(): string | null {
	if (typeof window === 'undefined') return null;
	const params = new URLSearchParams(window.location.search);
	const urlToken = params.get('token');
	if (urlToken) {
		localStorage.setItem('ikenga_auth_token', urlToken);
		return urlToken;
	}
	return localStorage.getItem('ikenga_auth_token');
}

export class TauriTransport implements RpcTransport {
	async invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
		const { invoke } = await import('@tauri-apps/api/core');
		return invoke<T>(cmd, args);
	}

	async listen<T>(event: string, handler: (event: { event: string; payload: T }) => void): Promise<() => void> {
		const { listen } = await import('@tauri-apps/api/event');
		return listen<T>(event, handler);
	}
}

export class WebRemoteTransport implements RpcTransport {
	private eventListeners: Map<string, Set<(event: { event: string; payload: any }) => void>> = new Map();

	async invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
		const token = getAuthToken();
		const headers: Record<string, string> = {
			'Content-Type': 'application/json',
		};
		if (token) {
			headers['Authorization'] = `Bearer ${token}`;
		}

		const res = await fetch('/api/rpc', {
			method: 'POST',
			headers,
			body: JSON.stringify({ cmd, args: args ?? {} }),
		});
		if (!res.ok) {
			throw new Error(`HTTP RPC error: ${res.status} ${res.statusText}`);
		}
		const json = await res.json();
		if (!json.ok) {
			throw new Error(json.error || `RPC command '${cmd}' failed`);
		}
		return json.data as T;
	}

	async listen<T>(event: string, handler: (event: { event: string; payload: T }) => void): Promise<() => void> {
		let listeners = this.eventListeners.get(event);
		if (!listeners) {
			listeners = new Set();
			this.eventListeners.set(event, listeners);
		}
		listeners.add(handler);

		return () => {
			listeners?.delete(handler);
		};
	}

	openPtySocket(id: string): WebSocket {
		const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
		const token = getAuthToken();
		const query = token ? `?token=${encodeURIComponent(token)}` : '';
		const ws = new WebSocket(`${protocol}//${window.location.host}/ws/pty/${encodeURIComponent(id)}${query}`);
		ws.binaryType = 'arraybuffer';
		return ws;
	}
}

let transportInstance: RpcTransport | null = null;

export function getTransport(): RpcTransport {
	if (!transportInstance) {
		if (isTauri()) {
			transportInstance = new TauriTransport();
		} else {
			transportInstance = new WebRemoteTransport();
		}
	}
	return transportInstance;
}
