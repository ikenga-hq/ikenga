// Where a terminal's claude statusline snapshots, hook settings and permission
// gate come from: the iyke bridge on the desktop, the daemon's own arms in a
// browser session (`server/term_hooks.rs`; remote-access gap audit rank 11).
// Callers ask here and never branch on the host themselves.

import { useSyncExternalStore } from 'react';
import {
	type TermHooksInfo,
	termHooksDecide,
	termHooksInfo,
	termHooksStatuslineSnapshot,
} from '@/lib/tauri-cmd';
import { isRemoteWebSession } from '@/lib/transport';
import { iykeFetch } from './client';

/** Every terminal's latest statusline snapshot, or null when none can be read. */
export async function fetchStatuslineSnapshots<T>(): Promise<Record<string, T> | null> {
	if (isRemoteWebSession()) {
		try {
			return (await termHooksStatuslineSnapshot()) as Record<string, T>;
		} catch (err) {
			const reason = err instanceof Error ? err.message : typeof err === 'string' ? err : null;
			if (reason && !info) {
				publish({ settingsDir: null, reason });
			}
			return null;
		}
	}
	const res = await iykeFetch('/iyke/statusline/snapshot');
	return res.ok ? ((await res.json()) as Record<string, T>) : null;
}

/** Answer a held gate in a browser session. Resolves to whether the gate was
 *  still waiting (`false`: already over); rejects with the daemon's refusal. */
export async function decideHookGateRemote(
	requestId: string,
	decision: 'approved' | 'denied'
): Promise<boolean> {
	return (await termHooksDecide(requestId, decision)).gated;
}

// ── Whether the daemon can wire claude's hooks at all ────────────────────────

/** Shown where a daemon that cannot take hooks would otherwise leave a panel
 *  "listening" for ever. */
const UNKNOWN_REASON = 'Not available on this server';

let info: TermHooksInfo | null = null;
let inFlight: Promise<TermHooksInfo> | null = null;
const listeners = new Set<() => void>();

function publish(next: TermHooksInfo): TermHooksInfo {
	info = next;
	for (const l of listeners) l();
	return next;
}

/**
 * Ask the daemon, once per page, where per-terminal hook settings go. Any
 * answer is final — including a refusal and a daemon too old to know the arm —
 * so a server that cannot do this is asked once, not on every launch.
 */
export function primeRemoteHooksInfo(): Promise<TermHooksInfo> {
	if (info) return Promise.resolve(info);
	inFlight ??= termHooksInfo()
		.then((i) =>
			publish({
				settingsDir: i.settingsDir ?? null,
				reason: i.settingsDir ? null : (i.reason ?? UNKNOWN_REASON),
			})
		)
		.catch(() => publish({ settingsDir: null, reason: UNKNOWN_REASON }));
	return inFlight;
}

/** Why a browser session's terminals get no statusline telemetry or hook
 *  events, or null (available, or not known yet). */
export function claudeHooksUnavailableReason(): string | null {
	return info?.reason ?? null;
}

function subscribe(cb: () => void): () => void {
	listeners.add(cb);
	return () => listeners.delete(cb);
}

/** React view of {@link claudeHooksUnavailableReason}. */
export function useClaudeHooksUnavailableReason(): string | null {
	return useSyncExternalStore(subscribe, claudeHooksUnavailableReason, () => null);
}

/** Test seam. */
export function __resetTerminalHooksForTests(): void {
	info = null;
	inFlight = null;
	listeners.clear();
}
