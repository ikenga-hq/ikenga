// WP-P9 — TanStack Query surface for in-app server updates.
//
// `useServerUpdate()` is the one read: the banner and the Settings › About
// panel share it. It is enabled only where `fetchServerUpdate` could answer
// (a browser tab; under T1 an admin). It polls every 2 s while root is
// applying an update (or a request waits to be claimed), and every 10 min
// otherwise, plus on window focus.
//
// The update restarts the server, so polls fail for a few seconds mid-run.
// `fetchServerUpdate` throws on those (rather than answering `null`), which
// keeps the last view on screen — no flicker — until the restarted server
// answers. The restarted server reports its new `current`; once that differs
// from the version this tab first saw, the SPA bundle it loaded is stale too
// (`dist/` ships with the server), so the UI offers a reload.

import { useQuery } from '@tanstack/react-query';
import { queryKeys } from '@/lib/query-keys';
import {
	fetchServerUpdate,
	isServerUpdateInFlight,
	mayAskForServerUpdates,
	type ServerUpdateView,
} from '@/lib/transport/server-update';

const FAST_POLL_MS = 2_000;
const SLOW_POLL_MS = 10 * 60 * 1_000;

/** The server version this tab first saw (its SPA bundle shipped with it). */
let loadedServerVersion: string | null = null;

function remember(view: ServerUpdateView | null): ServerUpdateView | null {
	if (view && loadedServerVersion === null) loadedServerVersion = view.current;
	return view;
}

/** True once the server runs a different version than the one this tab
 *  loaded with, after a successful in-app update. */
export function needsReload(view: ServerUpdateView | null | undefined): boolean {
	if (!view || loadedServerVersion === null) return false;
	return (
		view.current !== loadedServerVersion &&
		view.last_run?.state === 'succeeded' &&
		view.last_run.to === view.current
	);
}

export function serverUpdatePollInterval(view: ServerUpdateView | null | undefined): number {
	return isServerUpdateInFlight(view) ? FAST_POLL_MS : SLOW_POLL_MS;
}

export function useServerUpdate() {
	return useQuery({
		queryKey: queryKeys.serverUpdate.status(),
		queryFn: async () => remember(await fetchServerUpdate()),
		enabled: mayAskForServerUpdates(),
		refetchInterval: (query) => serverUpdatePollInterval(query.state.data),
		refetchOnWindowFocus: true,
		retry: false,
		staleTime: FAST_POLL_MS,
	});
}

/** Test-only. */
export function __resetServerUpdateForTests(version: string | null = null): void {
	loadedServerVersion = version;
}
