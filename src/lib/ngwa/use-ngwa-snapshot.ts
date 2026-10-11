// TanStack Query hook for the Ngwa unified snapshot (WP-15 / Gate G-NGWA-ITEM).
//
// Wraps `ngwaSnapshot()` from `@/lib/tauri-cmd` with:
// 1. Single query key to prevent duplicate concurrent cold scans (which take ~70s).
// 2. 60-second stale time, no refetch on window focus, no retry.
// 3. Frontend enrichment layer joining with `useRegistryIndex()`.
// 4. Per-source health tracking (Gate §2).

import { useMemo } from 'react';
import { useQuery } from '@tanstack/react-query';
import { ngwaSnapshot } from '@/lib/tauri-cmd';
import { useRegistryIndex } from '@/lib/registry/use-registry';
import { isServerLimit, type NgwaSourceHealthView } from './server-limits';
import {
	enrichNgwaItems,
	markTrustUnavailable,
	type NgwaStoreEntry,
	buildStoreCatalog,
} from './enrichment';
import type { NgwaItem, NgwaSnapshot } from '@ikenga/contract';

export const ngwaSnapshotQueryKey = ['ngwa', 'snapshot'] as const;

export interface UseNgwaSnapshotResult {
	snapshot: NgwaSnapshot | null;
	items: NgwaItem[];
	storeCatalog: NgwaStoreEntry[];
	sources: NgwaSnapshot['sources'] | null;
	/** Sources whose `ok` is false. `unavailable` = the server does not run
	 *  that source at all (the daemon's typed `unavailable` marker), as
	 *  opposed to one that failed to read. */
	unreadableSources: Array<{ source: string; error: string | null; unavailable: boolean }>;
	isLoading: boolean;
	error: Error | null;
	refetch: () => void;
}

export function useNgwaSnapshot(): UseNgwaSnapshotResult {
	const snapshotQuery = useQuery({
		queryKey: ngwaSnapshotQueryKey,
		queryFn: ngwaSnapshot,
		staleTime: 60_000,
		gcTime: 300_000,
		refetchOnWindowFocus: false,
		retry: false,
	});

	const registryQuery = useRegistryIndex();
	const registryEntries = registryQuery.data?.index.pkgs ?? [];

	const rawItems = snapshotQuery.data?.items ?? [];
	const trustHealth = snapshotQuery.data?.sources.trust as NgwaSourceHealthView | undefined;
	const trustUnavailable =
		trustHealth && !trustHealth.ok && isServerLimit(trustHealth) ? trustHealth.error : null;

	const items = useMemo(() => {
		return markTrustUnavailable(enrichNgwaItems(rawItems, registryEntries), trustUnavailable);
	}, [rawItems, registryEntries, trustUnavailable]);

	const storeCatalog = useMemo(() => {
		return buildStoreCatalog(items, registryEntries);
	}, [items, registryEntries]);

	const sources = snapshotQuery.data?.sources ?? null;

	const unreadableSources = useMemo(() => {
		if (!sources) return [];
		return (Object.entries(sources) as Array<[keyof NgwaSnapshot['sources'], NgwaSourceHealthView]>)
			.filter(([_, health]) => !health.ok)
			.map(([source, health]) => ({
				source,
				error: health.error,
				unavailable: isServerLimit(health),
			}));
	}, [sources]);

	return {
		snapshot: snapshotQuery.data ?? null,
		items,
		storeCatalog,
		sources,
		unreadableSources,
		isLoading: snapshotQuery.isLoading,
		error: snapshotQuery.error as Error | null,
		refetch: snapshotQuery.refetch,
	};
}
