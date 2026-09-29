// TanStack Query hooks for the Ikenga registry.
//
// Cadence mirrors `useUpdater`: index is fetched on mount, kept fresh for 6h,
// and silently refetched in the background on the same interval. The `Browse`
// route triggers a manual refetch on open (handled by the route, not here).
//
// Per-pkg detail files are fetched lazily — only the pkg the user is currently
// inspecting hits the network. Details are cached for the session; a manual
// refresh on the Browse route invalidates them along with the index.
//
// Failure model: TanStack `error` is surfaced to the UI. We do NOT silently
// fall back to a stale cached index — if signature verification or HTTP fails,
// the user sees the error. The whole point of the signed index is to refuse
// to use a tampered or unverifiable one.

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
	fetchIndex,
	fetchPkgDetail,
	resolveInstallPlan,
	type FetchedIndex,
	type InstallStep,
	type PkgDetail,
	type RegistryEntry,
} from './client';
import { cachedDetailGetter } from './install-plan';

const SIX_HOURS_MS = 6 * 60 * 60 * 1000;

export const registryKeys = {
	all: ['registry'] as const,
	index: () => [...registryKeys.all, 'index'] as const,
	detail: (name: string) => [...registryKeys.all, 'detail', name] as const,
	plan: (name: string, version?: string) =>
		[...registryKeys.all, 'plan', name, version ?? 'latest'] as const,
};

/**
 * Fetch the signed registry index. Cached for 6h; background-refetches on
 * the same interval. The index has a couple-hundred-byte payload + one sig
 * verify — cheap enough that we could refetch every minute, but 6h matches
 * the bundle-updater rhythm and keeps the user's network quiet.
 */
export function useRegistryIndex() {
	return useQuery({
		queryKey: registryKeys.index(),
		queryFn: ({ signal }) => fetchIndex(signal),
		staleTime: SIX_HOURS_MS,
		refetchInterval: SIX_HOURS_MS,
		refetchOnWindowFocus: false,
		// Don't retry on signature failure — that's not a transient error.
		// One retry on network failure is enough; further hammering won't help
		// a flaky proxy.
		retry: 1,
	});
}

/**
 * Lazy per-pkg detail. Only fires when `enabled=true` (the UI passes the
 * currently-selected pkg name; everything else stays unfetched).
 */
export function useRegistryPkgDetail(
	indexUrl: string | undefined,
	entry: RegistryEntry | undefined
) {
	return useQuery({
		queryKey: registryKeys.detail(entry?.name ?? ''),
		queryFn: ({ signal }) => {
			if (!indexUrl || !entry) {
				throw new Error('useRegistryPkgDetail: indexUrl + entry required');
			}
			return fetchPkgDetail(indexUrl, entry, signal);
		},
		enabled: Boolean(indexUrl && entry),
		staleTime: SIX_HOURS_MS,
	});
}

/**
 * Compute an install plan for the given root pkg detail. Resolves transitive
 * `@ikenga/pkg-*` deps through the same per-detail query cache, so any pkg
 * the user already inspected won't refetch.
 *
 * This is exposed as a mutation rather than a query because it has a clear
 * trigger ("user clicked Install / Update") and we want fresh resolution
 * each time — installing the same pkg twice in one session shouldn't reuse
 * a possibly-stale plan.
 */
export function useInstallPlanResolver(indexUrl: string | undefined) {
	const queryClient = useQueryClient();

	const getDetail = cachedDetailGetter(queryClient, indexUrl);

	return useMutation({
		mutationFn: async (args: { root: PkgDetail; version?: string }): Promise<InstallStep[]> => {
			return resolveInstallPlan(args.root, getDetail, args.version);
		},
	});
}

/**
 * Invalidate everything registry-related. Used by the "Refresh" button on
 * the Browse route and after a successful install (to pick up the new
 * latest version, if a release shipped between fetches).
 */
export function useRefreshRegistry() {
	const queryClient = useQueryClient();
	return () => {
		void queryClient.invalidateQueries({ queryKey: registryKeys.all });
	};
}

/** Re-export for routes that want the typed `FetchedIndex` shape. */
export type { FetchedIndex, InstallStep, PkgDetail, RegistryEntry };
