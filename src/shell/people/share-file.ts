// WP-11: "Share…" on a file. The Files row menu and the viewer pane menu call
// `requestShareFile(path)`; `ShareFileHost` (share-file-host.tsx, mounted in
// the session chrome) opens the invite sheet with the artifact scope selected
// and the path filled in. `useCanShareFile` decides whether the item shows.

import { useEffect, useState } from 'react';
import { create } from 'zustand';

import { type AccessStatus, accessStatus } from '@/lib/access/client';
import { useShellStore } from '@/lib/shell/shell-store';
import { currentShare } from '@/lib/transport';

import { shareDisabledReason, tierOf } from './members';

/**
 * `abs` as a project-relative, `/`-separated artifact path, or `null` when it
 * is not strictly inside `root` (a share is confined to the project folder, so
 * a file outside it cannot be shared).
 */
export function projectRelativePath(abs: string, root: string | null | undefined): string | null {
	if (!root) return null;
	const a = abs.replace(/\\/g, '/');
	const r = root.replace(/\\/g, '/').replace(/\/+$/, '');
	if (!r || !a.startsWith(`${r}/`)) return null;
	const rel = a.slice(r.length + 1);
	if (!rel || rel.split('/').some((p) => p === '..')) return null;
	return rel;
}

/** Pure gate: may this window offer "Share…" for `path`? */
export function canShareFile(f: {
	tier: 't0' | 't1';
	adminStrength: boolean | null;
	inShare: boolean;
	root: string | null | undefined;
	path: string;
}): boolean {
	// Hidden inside a share for members: an artifact is shared from your own workspace.
	if (f.inShare) return false;
	if (shareDisabledReason({ tier: f.tier, adminStrength: f.adminStrength, shareRole: null, inShare: false, hasFolder: Boolean(f.root?.trim()) }) !== null)
		return false;
	return projectRelativePath(f.path, f.root) !== null;
}

interface ShareFileState {
	/** Project-relative path to prefill, or null when the sheet is closed. */
	path: string | null;
	open: (relativePath: string) => void;
	close: () => void;
}

export const useShareFileStore = create<ShareFileState>((set) => ({
	path: null,
	open: (path) => set({ path }),
	close: () => set({ path: null }),
}));

/** Open the invite sheet for the file at absolute `abs`; false when it is outside the project. */
export function requestShareFile(abs: string): boolean {
	const rel = projectRelativePath(abs, useShellStore.getState().activeProject?.root_path);
	if (rel === null) return false;
	useShareFileStore.getState().open(rel);
	return true;
}

// One access_status call for every row that asks, refreshed at most once a minute.
let statusCache: { at: number; p: Promise<AccessStatus | null> } | null = null;
function cachedStatus(): Promise<AccessStatus | null> {
	if (!statusCache || Date.now() - statusCache.at > 60_000) {
		statusCache = { at: Date.now(), p: Promise.resolve()
				.then(() => accessStatus())
				.catch(() => null) };
	}
	return statusCache.p;
}

/** Whether to show "Share…" for the file at absolute `path`. */
export function useCanShareFile(path: string | undefined): boolean {
	const root = useShellStore((s) => s.activeProject?.root_path);
	const [status, setStatus] = useState<AccessStatus | null | undefined>(undefined);
	useEffect(() => {
		let cancelled = false;
		void cachedStatus().then((s) => {
			if (!cancelled) setStatus(s);
		});
		return () => {
			cancelled = true;
		};
	}, []);
	if (!path || status === undefined) return false;
	return canShareFile({
		tier: tierOf(status),
		adminStrength: status ? status.adminStrength : null,
		inShare: currentShare() != null,
		root,
		path,
	});
}
