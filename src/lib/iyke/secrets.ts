// Client for the /iyke/secret/* bridge endpoints (Phase 7).
//
// The Settings → Secrets UI uses the Tauri commands directly
// (lower latency, same-process). This file exists for parity so the
// FE has the same surface external callers (CLI, MCP) get, and for
// future browser-pkg paths that talk through the bridge.

import type { SecretsLockState } from '@/lib/tauri-cmd';
import { iykeFetch } from './client';

/** Wire-format scope. Same shape the Rust handler parses:
 *  empty/missing → active project; `workspace` | `project:<id>` | `pkg:<id>`. */
export type IykeScopeString = '' | 'workspace' | `project:${string}` | `pkg:${string}`;

export interface IykeSecretEntry {
	key: string;
	scope: { kind: 'workspace' } | { kind: 'project'; id: string } | { kind: 'pkg'; id: string };
	value: string | null;
}

export async function iykeSecretGet(
	key: string,
	scope?: IykeScopeString
): Promise<IykeSecretEntry> {
	const qs = new URLSearchParams({ key });
	if (scope) qs.set('scope', scope);
	const res = await iykeFetch(`/iyke/secret/get?${qs.toString()}`);
	if (!res.ok) throw new Error(`iyke /iyke/secret/get ${res.status}: ${await res.text()}`);
	return (await res.json()) as IykeSecretEntry;
}

export async function iykeSecretSet(
	key: string,
	value: string,
	scope?: IykeScopeString
): Promise<void> {
	const res = await iykeFetch('/iyke/secret/set', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ scope: scope ?? null, key, value }),
	});
	if (!res.ok) throw new Error(`iyke /iyke/secret/set ${res.status}: ${await res.text()}`);
}

export async function iykeSecretDelete(key: string, scope?: IykeScopeString): Promise<void> {
	const res = await iykeFetch('/iyke/secret/delete', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ scope: scope ?? null, key }),
	});
	if (!res.ok) throw new Error(`iyke /iyke/secret/delete ${res.status}: ${await res.text()}`);
}

export async function iykeSecretsLockState(): Promise<SecretsLockState> {
	const res = await iykeFetch('/iyke/secrets/lock-state');
	if (!res.ok) throw new Error(`iyke /iyke/secrets/lock-state ${res.status}: ${await res.text()}`);
	return (await res.json()) as SecretsLockState;
}

export const iykeSecretLockState = iykeSecretsLockState;

export async function iykeSecretList(scope?: IykeScopeString): Promise<{ keys: string[] }> {
	const qs = new URLSearchParams();
	if (scope) qs.set('scope', scope);
	const q = qs.toString();
	const res = await iykeFetch(`/iyke/secret/list${q ? `?${q}` : ''}`);
	if (!res.ok) throw new Error(`iyke /iyke/secret/list ${res.status}: ${await res.text()}`);
	return (await res.json()) as { keys: string[] };
}
