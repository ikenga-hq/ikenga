// Pkgs that are on disk but failed to register (manifest won't load, or a
// registry rejected them at boot). The kernel's install-health scan reports
// them as `pkgs_dir_unloadable` / `register_failed`; Ngwa Health lists them
// with "Reinstall from registry", and the Store marks their row installed but
// broken so its primary action reads Reinstall instead of a plain Install.
//
// Shares the Health surface's query key (`['pkg', 'health']`), so a Store
// install — which invalidates the whole `['pkg']` family — refreshes both.

import { useMemo } from 'react';
import { useQuery } from '@tanstack/react-query';
import { entryMatchesPkgId } from '@/lib/registry/use-updates-available';
import { isUnregisteredPkgIssue, pkgHealthScan, type PkgHealthIssue } from '@/lib/tauri-cmd';
import type { NgwaStoreEntry } from './enrichment';

export const pkgHealthQueryKey = ['pkg', 'health'] as const;

/** Unregistered-pkg issues keyed by pkg id (first issue wins). */
export function brokenPkgMap(issues: readonly PkgHealthIssue[]): Map<string, PkgHealthIssue> {
	const out = new Map<string, PkgHealthIssue>();
	for (const i of issues) {
		if (isUnregisteredPkgIssue(i.issue) && !out.has(i.id)) out.set(i.id, i);
	}
	return out;
}

/** Does registry npm name `name` stand for manifest id `pkgId`? */
export function registryNameMatches(name: string, pkgId: string): boolean {
	return name === pkgId || entryMatchesPkgId(name, pkgId);
}

/** Stamp `broken` (the scan's detail) on every Store entry whose pkg is on
 *  disk but unregistered. Entries are returned as-is when nothing matches. */
export function markBrokenEntries(
	entries: NgwaStoreEntry[],
	broken: ReadonlyMap<string, PkgHealthIssue>
): NgwaStoreEntry[] {
	if (broken.size === 0) return entries;
	return entries.map((e) => {
		for (const [pkgId, issue] of broken) {
			if (registryNameMatches(e.name, pkgId)) return { ...e, broken: issue.detail };
		}
		return e;
	});
}

/** The install-health scan, reduced to the broken (unregistered) pkgs. A
 *  failed scan reads as "none known" — the Store must still work. */
export function useBrokenPkgs(): ReadonlyMap<string, PkgHealthIssue> {
	const q = useQuery({ queryKey: pkgHealthQueryKey, queryFn: pkgHealthScan, retry: false });
	return useMemo(() => brokenPkgMap(q.data ?? []), [q.data]);
}
