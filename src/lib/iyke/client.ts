// Self-introspection client for the in-app frontend. Fetches the Iyke
// endpoint from the Rust runtime via a Tauri command, then talks to the
// HTTP server using the same bearer-token contract as external CLI/MCP
// clients — that way one bug surface, one protocol.
//
// The endpoint is cached for the page lifetime: the server is bound for
// the duration of the app process, so port + token never change without
// a full reload.

import { invoke } from '@/lib/transport';

import type { IykeEndpoint, IykeStateResponse } from './types';

let cachedEndpoint: IykeEndpoint | null = null;
let inFlight: Promise<IykeEndpoint> | null = null;

export async function getEndpoint(): Promise<IykeEndpoint> {
	if (cachedEndpoint) return cachedEndpoint;
	if (inFlight) return inFlight;
	inFlight = (async () => {
		const ep = await invoke<IykeEndpoint>('iyke_endpoint');
		cachedEndpoint = ep;
		return ep;
	})();
	try {
		return await inFlight;
	} finally {
		inFlight = null;
	}
}

export async function iykeFetch(path: string, init?: RequestInit): Promise<Response> {
	const ep = await getEndpoint();
	const headers = new Headers(init?.headers);
	headers.set('Authorization', `Bearer ${ep.token}`);
	return fetch(`${ep.url}${path}`, { ...init, headers });
}

export async function getState(): Promise<IykeStateResponse> {
	const res = await iykeFetch('/iyke/state');
	if (!res.ok) {
		throw new Error(`iyke /iyke/state ${res.status} ${res.statusText}`);
	}
	return (await res.json()) as IykeStateResponse;
}

/**
 * Push the current sidebar mode + route + pane snapshot into the
 * Rust-side mirror so `/iyke/state` can answer questions.
 * `null`/omitted fields leave the existing value untouched (partial
 * update semantics).
 */
export async function setShell(args: {
	mode?: string | null;
	route?: string | null;
	panes?: unknown;
	sidebarCollapsed?: boolean | null;
}): Promise<void> {
	return invoke('iyke_set_shell', {
		mode: args.mode ?? null,
		route: args.route ?? null,
		panes: args.panes ?? null,
		sidebarCollapsed: args.sidebarCollapsed ?? null,
	});
}
