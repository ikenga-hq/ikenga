// honest-failure-states WP-2 — TanStack Query surface for `wsl_health_probe`.
//
// D-7: probe before each WSL launch and whenever a WSL session prints a
// network errno; no background polling, no boot probe. So:
//   - `prelaunchWslProbe` (session-store `openTabPty`) fetches through the
//     cache (30 s, matching the backend's own per-distro cache);
//   - `reportWslNetworkErrno` (the PTY errno scanner) forces one fresh probe
//     per episode, debounced;
//   - `useWslHealth` reads the same cache. Surfaces that shouldn't probe on
//     their own (the terminal-pane banner) pass `enabled: false` and only
//     show what a launch or errno already measured.
// A fresh probe records / resolves the `fix.wsl_network` notification on the
// Rust side; a fresh `ok` here also ends the episode in the UI store.

import { queryOptions, useQuery } from '@tanstack/react-query';
import { queryClient } from '@/lib/query-client';
import { type WslHealth, wslHealthProbe } from '@/lib/tauri-cmd';
import { shouldForceProbeOnErrno } from './errno';
import { useWslHealthUi } from './store';
import { normalizeWslDistro } from './tabs';

/** Matches the backend's per-distro cache. */
export const WSL_HEALTH_STALE_MS = 30_000;

export function wslHealthQueryKey(distro: string | null | undefined) {
	return ['wsl', 'health', normalizeWslDistro(distro)] as const;
}

function settle(distro: string, health: WslHealth): WslHealth {
	if (health.state === 'ok') useWslHealthUi.getState().endEpisode(distro);
	return health;
}

export function wslHealthQueryOptions(distro: string | null | undefined) {
	const key = normalizeWslDistro(distro);
	return queryOptions({
		queryKey: wslHealthQueryKey(key),
		queryFn: async () => settle(key, await wslHealthProbe({ distro: key })),
		staleTime: WSL_HEALTH_STALE_MS,
		retry: false,
		refetchOnWindowFocus: false,
		refetchOnReconnect: false,
	});
}

/** The cached health of a distro; probes (through the 30 s cache) only when
 *  `enabled`. */
export function useWslHealth(distro: string | null | undefined, opts: { enabled?: boolean } = {}) {
	return useQuery({ ...wslHealthQueryOptions(distro), enabled: opts.enabled ?? true });
}

/** Probe now. `force` bypasses both caches; otherwise a result younger than
 *  30 s is reused. Rejects only on an invalid distro name / IPC failure. */
export async function probeWslHealth(
	distro: string | null | undefined,
	opts: { force?: boolean } = {}
): Promise<WslHealth> {
	const key = normalizeWslDistro(distro);
	if (!opts.force) return queryClient.fetchQuery(wslHealthQueryOptions(key));
	const health = settle(key, await wslHealthProbe({ distro: key, force: true }));
	queryClient.setQueryData(wslHealthQueryKey(key), health);
	return health;
}

/** Put a result measured elsewhere (a fix's re-probe) into the cache. */
export function setWslHealth(distro: string | null | undefined, health: WslHealth): void {
	const key = normalizeWslDistro(distro);
	queryClient.setQueryData(wslHealthQueryKey(key), settle(key, health));
}

/** D-7 pre-launch probe. Fire-and-forget: never blocks or fails a spawn. */
export function prelaunchWslProbe(distro: string | null | undefined): void {
	probeWslHealth(distro).catch((err) => {
		console.warn('[wsl-health] pre-launch probe failed', err);
	});
}

/** A WSL tab printed a network errno: force one probe per episode. */
export function reportWslNetworkErrno(distro: string | null | undefined, now = Date.now()): void {
	const key = normalizeWslDistro(distro);
	const queryKey = wslHealthQueryKey(key);
	const cached = queryClient.getQueryData<WslHealth>(queryKey);
	const ui = useWslHealthUi.getState();
	const go = shouldForceProbeOnErrno({
		cachedState: cached?.state,
		lastForcedAt: ui.errnoProbedAt[key],
		inFlight: queryClient.isFetching({ queryKey }) > 0,
		now,
	});
	if (!go) return;
	ui.markErrnoProbe(key, now);
	probeWslHealth(key, { force: true }).catch((err) => {
		console.warn('[wsl-health] errno probe failed', err);
	});
}
