// Resolves a pkg id or ngwa-item id (as found in `/pkg/<id>` and
// `/ngwa/item/<id>` route paths) to its human display name, so pane tabs,
// the address bar, and the ⌘K switcher show "Studio" instead of
// "com.ikenga.studio".
//
// Two sources, both read from data already on the frontend rather than
// fetched fresh for this purpose:
//   - Installed pkgs' manifest name, via `usePkgsDerived()` — the same
//     query-backed hook the pkg surface already renders from, so this adds
//     no new network traffic beyond what's already cached/in flight there.
//   - Ngwa items that aren't pkgs (skills, agents, …): `NgwaItem.display_name`
//     from the ngwa snapshot query, read with `enabled: false` so a tab strip
//     mounting before anything else has fetched the snapshot never triggers
//     the cold scan (`useNgwaSnapshot`'s own docs call out ~70s on a cold
//     cache) — this only reads whatever's already in the query cache, and
//     stays reactive once that query is fetched elsewhere (e.g. the Ngwa list
//     view already on screen, or opened earlier this session).

import { useCallback, useMemo } from 'react';
import { useQuery } from '@tanstack/react-query';
import { ngwaSnapshot } from '@/lib/tauri-cmd';
import { ngwaSnapshotQueryKey } from '@/lib/ngwa/use-ngwa-snapshot';
import { usePkgsDerived } from '@/lib/pkgs/use-derived';

/** Given a decoded pkg/ngwa-item id, returns its display name, or
 *  `undefined` when nothing resolves it (caller falls back to the raw id). */
export type PaneDisplayNameResolver = (id: string) => string | undefined;

export function usePaneDisplayNameResolver(): PaneDisplayNameResolver {
	const pkgs = usePkgsDerived();

	// Cache-only read: `enabled: false` means this never itself triggers a
	// fetch (and so never pays the cold-scan cost) — it only subscribes to
	// whatever `['ngwa', 'snapshot']` already holds, staying reactive if some
	// other mounted consumer (e.g. `useNgwaSnapshot()` on an Ngwa route)
	// populates or refreshes it.
	const ngwaQuery = useQuery({
		queryKey: ngwaSnapshotQueryKey,
		queryFn: ngwaSnapshot,
		enabled: false,
		staleTime: 60_000,
	});

	const pkgNameById = useMemo(() => {
		const map = new Map<string, string>();
		for (const row of pkgs.installed) map.set(row.id, row.name);
		return map;
	}, [pkgs.installed]);

	const ngwaNameById = useMemo(() => {
		const map = new Map<string, string>();
		for (const item of ngwaQuery.data?.items ?? []) map.set(item.id, item.display_name);
		return map;
	}, [ngwaQuery.data]);

	return useCallback(
		(id: string) => pkgNameById.get(id) ?? ngwaNameById.get(id),
		[pkgNameById, ngwaNameById]
	);
}
