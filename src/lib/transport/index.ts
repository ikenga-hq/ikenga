export interface RpcTransport {
	invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T>;
	listen<T>(event: string, handler: (event: { event: string; payload: T }) => void): Promise<() => void>;
	openPtySocket?(id: string): WebSocket;
}

export function isTauri(): boolean {
	if (typeof window === 'undefined') return false;
	return '__TAURI_INTERNALS__' in window || '__TAURI__' in window;
}

const TOKEN_KEY = 'ikenga_auth_token';

/** Primary home for the token: memory, so an XSS payload has to be running
 *  at the same time as us to reach it rather than reading it back later. */
let cachedToken: string | null = null;
let tokenHydrated = false;

/**
 * Read the remote-access token.
 *
 * The token arrives as `?token=…` on the opening link, which is the worst
 * place for a credential to stay: it lands in browser history, in the
 * `Referer` of any outbound request, and in every server access log that
 * records query strings. So the first read consumes it — strips it from the
 * URL via `replaceState` and keeps it in memory, with `sessionStorage` as
 * the reload backing. `sessionStorage` (not `localStorage`) scopes it to
 * this tab and clears it when the tab closes; a new tab needs the link
 * again, which is the right trade for a token that grants a shell.
 */
export function getAuthToken(): string | null {
	if (typeof window === 'undefined') return null;
	if (cachedToken) return cachedToken;

	if (!tokenHydrated) {
		tokenHydrated = true;

		// Earlier builds persisted the token to localStorage, where it
		// outlived the session. Clear any leftover from those.
		try {
			localStorage.removeItem(TOKEN_KEY);
		} catch {
			// Storage can throw outright (Safari private mode, blocked
			// site data); nothing here is worth failing a page load over.
		}

		const url = new URL(window.location.href);
		const urlToken = url.searchParams.get('token');
		if (urlToken) {
			cachedToken = urlToken;
			try {
				sessionStorage.setItem(TOKEN_KEY, urlToken);
			} catch {
				// Memory-only for this page load; a reload will need the link.
			}
			url.searchParams.delete('token');
			window.history.replaceState(null, '', url.toString());
			return cachedToken;
		}

		try {
			cachedToken = sessionStorage.getItem(TOKEN_KEY);
		} catch {
			cachedToken = null;
		}
	}

	return cachedToken;
}

/** Drop the token from memory and this tab's storage. */
export function clearAuthToken(): void {
	cachedToken = null;
	tokenHydrated = true;
	try {
		sessionStorage.removeItem(TOKEN_KEY);
		localStorage.removeItem(TOKEN_KEY);
	} catch {
		// Nothing to do — the in-memory copy is already gone.
	}
}

export class TauriTransport implements RpcTransport {
	async invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
		const { invoke } = await import('@tauri-apps/api/core');
		// Forward the no-args shape as `invoke(cmd)`, not `invoke(cmd, undefined)` —
		// the two are equivalent to Tauri but not to a spy asserting arity.
		return args === undefined ? invoke<T>(cmd) : invoke<T>(cmd, args);
	}

	async listen<T>(event: string, handler: (event: { event: string; payload: T }) => void): Promise<() => void> {
		const { listen } = await import('@tauri-apps/api/event');
		return listen<T>(event, handler);
	}
}

export class WebRemoteTransport implements RpcTransport {
	private eventListeners: Map<string, Set<(event: { event: string; payload: unknown }) => void>> = new Map();
	private warnedEvents: Set<string> = new Set();

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
			if (res.status === 401) {
				try {
					const { useReauthStore } = await import('./reauth-store');
					useReauthStore.getState().showReauth();
				} catch {
					// Fall through
				}
			}
			throw new Error(`HTTP RPC error: ${res.status} ${res.statusText}`);
		}
		const json = await res.json();
		if (!json.ok) {
			throw new Error(json.error || `RPC command '${cmd}' failed`);
		}
		return json.data as T;
	}

	/**
	 * Register an event handler.
	 *
	 * ⚠ Nothing delivers events to a browser client yet. The headless daemon
	 * has no counterpart to Tauri's event bus — `emit` on the Rust side goes
	 * to an `AppHandle` that does not exist here — so every subscription made
	 * through this transport stays silent. That is a real functional gap for
	 * the ~30 `listen()`-based features in `tauri-cmd.ts` (fs watchers,
	 * `projects:active-changed`, the pa-action approve-gate, runtime events);
	 * `ptyListen` is the one exception, and it bypasses this path for a
	 * dedicated WebSocket.
	 *
	 * Handlers are still registered so that the moment a producer lands it
	 * can fan out through {@link WebRemoteTransport.dispatch}. Until then
	 * each distinct event name warns once, so a dead subscription shows up in
	 * the console instead of being mistaken for "no events happened".
	 */
	async listen<T>(event: string, handler: (event: { event: string; payload: T }) => void): Promise<() => void> {
		if (!this.warnedEvents.has(event)) {
			this.warnedEvents.add(event);
			console.warn(
				`[transport] listen('${event}') has no event source in browser mode — ` +
					'this subscription will never fire. See WebRemoteTransport.listen.'
			);
		}

		let listeners = this.eventListeners.get(event);
		if (!listeners) {
			listeners = new Set();
			this.eventListeners.set(event, listeners);
		}
		const entry = handler as (event: { event: string; payload: unknown }) => void;
		listeners.add(entry);

		return () => {
			listeners?.delete(entry);
		};
	}

	/**
	 * Deliver an event to everything registered for `name`. The single entry
	 * point for a future server-side event channel; nothing calls it yet.
	 */
	dispatch(name: string, payload: unknown): void {
		const listeners = this.eventListeners.get(name);
		if (!listeners) return;
		for (const handler of listeners) {
			handler({ event: name, payload });
		}
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

/**
 * True only for a page served by `ikenga-server` to a browser.
 *
 * `!isTauri()` alone is not that test — it is also true under jsdom, in a
 * Node import, and in any harness that stubs `@tauri-apps/api`, and routing
 * those through the HTTP transport turns a mocked `invoke` into a real
 * `fetch('/api/rpc')`. The daemon requires a token on every route and hands
 * it to the page on the opening link, so a token in this tab is the marker
 * that we are actually talking to one. Without it there is nothing to talk
 * to and the desktop transport is the right default.
 */
export function isRemoteWebSession(): boolean {
	return !isTauri() && getAuthToken() !== null;
}

export function getTransport(): RpcTransport {
	if (!transportInstance) {
		transportInstance = isRemoteWebSession() ? new WebRemoteTransport() : new TauriTransport();
	}
	return transportInstance;
}
