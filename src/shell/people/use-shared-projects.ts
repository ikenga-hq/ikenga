// Shares for the project switcher (title-row chip + ⌘P palette).
//
// The switcher lists what `SharedWithYou` lists (`access_shares_list`) under
// the same condition: a T1 session that is not already inside a share. It is
// fetched lazily — only once the switcher is opened (`active`) — and cached
// for a short TTL so re-opening the popover does not refetch. Any failure
// (T0 desktop, no store, network) resolves to an empty list: the switcher
// shows no section and no error.

import { useEffect, useState } from 'react';

import { accessSharesList, accessStatus } from '@/lib/access/client';
import type { Project } from '@/lib/tauri-cmd';
import {
	currentShare,
	invokeOutsideShare,
	isRemoteWebSession,
	onShareModeChange,
	type ShareSelection,
} from '@/lib/transport';
import { isT1Session } from '@/lib/transport/t1-session';

import type { ShareView } from './shared-with-you';

const TTL_MS = 30_000;

let cache: { at: number; shares: ShareView[] } | null = null;
let inflight: Promise<ShareView[]> | null = null;

async function load(): Promise<ShareView[]> {
	try {
		let t1: boolean;
		try {
			t1 = (await accessStatus()).tier === 't1';
		} catch {
			t1 = isT1Session();
		}
		if (!t1) return [];
		const list = (await accessSharesList()) as ShareView[];
		return Array.isArray(list) ? list : [];
	} catch {
		return [];
	}
}

/** Shares to list in a switcher; `[]` until `active`, in a share, off T1, or on error. */
export function useSharedProjects(active: boolean): ShareView[] {
	const [shares, setShares] = useState<ShareView[]>(() => cache?.shares ?? []);
	useEffect(() => {
		if (!active || currentShare() || !isRemoteWebSession()) return;
		if (cache && Date.now() - cache.at < TTL_MS) {
			setShares(cache.shares);
			return;
		}
		let cancelled = false;
		inflight ??= load().finally(() => {
			inflight = null;
		});
		void inflight.then((list) => {
			cache = { at: Date.now(), shares: list };
			if (!cancelled) setShares(list);
		});
		return () => {
			cancelled = true;
		};
	}, [active]);
	return active && !currentShare() && isRemoteWebSession() ? shares : [];
}

/** The selected share, live. */
export function useCurrentShare(): ShareSelection | null {
	const [share, setShare] = useState<ShareSelection | null>(() => currentShare());
	useEffect(() => onShareModeChange(setShare), []);
	return share;
}

/**
 * The caller's OWN projects while inside a share. The store's `projects` is
 * loaded over the share transport, where the broker narrows `project_list` to
 * the shared project alone — useless for "Your projects". So in a share this
 * fetches the list once per `active` open with the share header omitted.
 *
 * Returns `null` while loading, `[]` on failure/none (callers then render no
 * list rather than a placeholder that never resolves). Outside a share it
 * returns `fallback` (the store's list) unchanged and fetches nothing.
 */
export function useOwnProjectsInShare(
	share: ShareSelection | null,
	active: boolean,
	fallback: Project[]
): Project[] | null {
	const [own, setOwn] = useState<Project[] | null>(null);
	useEffect(() => {
		if (!share || !active || !isRemoteWebSession()) return;
		let cancelled = false;
		setOwn(null);
		invokeOutsideShare<Project[]>('project_list', { includeArchived: true })
			.then((list) => {
				if (!cancelled) setOwn(Array.isArray(list) ? list : []);
			})
			.catch(() => {
				if (!cancelled) setOwn([]);
			});
		return () => {
			cancelled = true;
		};
	}, [share, active]);
	return share ? own : fallback;
}

/** Test-only. */
export function __resetSharedProjectsForTests(): void {
	cache = null;
	inflight = null;
}
