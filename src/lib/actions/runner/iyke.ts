// WP-53 — the `iyke` run kind (G-ACTIONS §8.1): a call to the in-app iyke
// bridge with its bearer token (`iykeFetch`, the same contract the CLI and
// MCP clients use). The route is NOT a template: the six variables travel
// in the JSON body for `POST` (the default) and as query parameters for
// `GET` (§8.2 — "values travel in the JSON body"; `method` is additive,
// §15 B-13).
//
// Only bridge paths are accepted: `/pane/navigate` and `/iyke/pane/navigate`
// both address `/iyke/pane/navigate`. A scheme, a host, `..` or a query
// string in the route is refused, so an action can never point the token
// at another origin.

import { iykeFetch } from '@/lib/iyke/client';
import type { ActionRun } from '../types';
import type { RunVariables } from './interpolate';

export type IykeRun = Extract<ActionRun, { kind: 'iyke' }>;

export interface IykeCallResult {
	ok: boolean;
	status: number;
	/** Parsed JSON when the response is JSON, else the text. */
	body: unknown;
}

/** The bridge path for a route, or null when the route is not one. */
export function iykePath(route: string): string | null {
	const trimmed = route.trim();
	if (!trimmed.startsWith('/') || trimmed.startsWith('//')) return null;
	if (/[?#\\\s]/.test(trimmed) || trimmed.includes('://')) return null;
	if (trimmed.split('/').some((segment) => segment === '..' || segment === '.')) return null;
	const path = trimmed === '/iyke' || trimmed.startsWith('/iyke/') ? trimmed : `/iyke${trimmed}`;
	return path;
}

export function iykeRequest(
	run: IykeRun,
	variables: RunVariables
): { path: string; init: RequestInit } | null {
	const base = iykePath(run.route);
	if (!base) return null;
	const method = run.method ?? 'POST';
	if (method === 'GET') {
		const query = new URLSearchParams(variables).toString();
		return { path: query ? `${base}?${query}` : base, init: { method: 'GET' } };
	}
	return {
		path: base,
		init: {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify(variables),
		},
	};
}

export async function callIyke(path: string, init: RequestInit): Promise<IykeCallResult> {
	const response = await iykeFetch(path, init);
	const text = await response.text();
	let body: unknown = text;
	if ((response.headers.get('content-type') ?? '').includes('json')) {
		try {
			body = JSON.parse(text);
		} catch {
			body = text;
		}
	}
	return { ok: response.ok, status: response.status, body };
}
