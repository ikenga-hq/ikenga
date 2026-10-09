// The admin "Server" card's read (`server_health`): one query, shared by
// whoever shows the card. Enabled only where the daemon can answer it: a
// browser tab, and under T1 an admin (a member is never sent to the arm;
// the broker would refuse it anyway). Refreshes every 12 s, and only while
// the tab is visible (TanStack pauses interval refetches in a background tab).

import { useQuery } from '@tanstack/react-query';
import { queryKeys } from '@/lib/query-keys';
import { type ServerHealth, SUPPORTED_SCHEMA } from '@/lib/server-health/model';
import { invoke, isBrowserSession } from '@/lib/transport';
import { currentPrincipal, isT1Session } from '@/lib/transport/t1-session';

export const SERVER_HEALTH_POLL_MS = 12_000;

/** Whether this tab may ask: a browser; under T1 an admin. Under T0 every
 *  browser credential is the owner's (the arm still refuses a share). */
export function mayAskForServerHealth(): boolean {
	if (!isBrowserSession()) return false;
	if (isT1Session()) return currentPrincipal()?.is_admin === true;
	return true;
}

/** `null` for a snapshot this client does not understand (a newer schema)
 *  rather than rendering it wrongly. */
export async function fetchServerHealth(): Promise<ServerHealth | null> {
	const h = await invoke<ServerHealth>('server_health');
	if (!h || typeof h !== 'object' || h.schema !== SUPPORTED_SCHEMA) return null;
	return h;
}

export function useServerHealth() {
	return useQuery({
		queryKey: queryKeys.serverHealth.snapshot(),
		queryFn: fetchServerHealth,
		enabled: mayAskForServerHealth(),
		refetchInterval: SERVER_HEALTH_POLL_MS,
		refetchIntervalInBackground: false,
		refetchOnWindowFocus: true,
		retry: false,
		staleTime: 5_000,
	});
}
