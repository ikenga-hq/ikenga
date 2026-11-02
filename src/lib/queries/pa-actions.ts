import { queryOptions } from '@tanstack/react-query';
import { queryKeys } from '@/lib/query-keys';
import { paActionsList } from '@/lib/tauri-cmd';

/** TanStack Query options for the gate's active draft set (or a status filter).
 *  The Outbox queue itself lives in the com.ikenga.outbound pkg (plans/outbox-redesign
 *  D-01), which reads through `host.paActions.list`; the shell keeps this for its entry
 *  points only — the status-bar / rail pending count and the Explorer's Outbox entry. */
export function paActionsListQueryOptions(status?: string) {
	return queryOptions({
		queryKey: queryKeys.paActions.list(status),
		queryFn: () => paActionsList(status),
		staleTime: 10_000,
	});
}
