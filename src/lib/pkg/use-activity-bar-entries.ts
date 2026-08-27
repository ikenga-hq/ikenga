// Activity-bar entries contributed by installed pkgs via manifest `ui.nav[0]`.
// Read from the pkg kernel snapshot and re-fetched on pkg install / uninstall /
// reload so newly-mounted pkgs appear (and removed ones disappear) without a
// shell restart.
//
// Shared by `activity-bar.tsx` (renders one rail icon per entry) and
// `sidebar.tsx` (resolves the head title for a `pkg:<id>` mode). The `loaded`
// flag lets callers distinguish "no pkgs installed" from "snapshot not fetched
// yet" — the activity bar needs that to avoid reconciling a persisted pkg mode
// to 'app' before the kernel snapshot has even arrived.

import { listen } from '@tauri-apps/api/event';
import { useEffect, useState } from 'react';
import { pkgKernelStatus } from '@/lib/tauri-cmd';

/** Shape mirrors the Rust `ActivityBarBadge` in
 *  `pkg/registries/activity_bar.rs` (WP-11). */
export interface PkgActivityBarBadge {
	dot: boolean;
	count?: number | null;
	tooltip?: string | null;
}

/** Shape mirrors the Rust `ActivityBarEntry` in
 *  `pkg/registries/activity_bar.rs`. */
export interface PkgActivityBarEntry {
	pkg_id: string;
	id: string;
	label: string;
	icon?: string | null;
	section?: string | null;
	route: string;
	badge?: PkgActivityBarBadge | null;
}

export interface PkgActivityBarState {
	entries: PkgActivityBarEntry[];
	/** True once the first kernel-snapshot fetch has resolved (success or
	 *  failure). Until then `entries` is an empty placeholder, not a real
	 *  "nothing installed" answer. */
	loaded: boolean;
}

export function usePkgActivityBarEntries(): PkgActivityBarState {
	const [entries, setEntries] = useState<PkgActivityBarEntry[]>([]);
	const [loaded, setLoaded] = useState(false);

	useEffect(() => {
		let cancelled = false;

		async function refresh() {
			try {
				const status = await pkgKernelStatus();
				const reg = (status.registries.activity_bar ?? {}) as {
					entries?: PkgActivityBarEntry[];
				};
				if (!cancelled) setEntries(reg.entries ?? []);
			} catch {
				if (!cancelled) setEntries([]);
			} finally {
				if (!cancelled) setLoaded(true);
			}
		}

		void refresh();

		// Kernel lifecycle events. The names match those emitted by the pkg
		// kernel in `lifecycle.rs` and `commands/pkg_dev.rs`.
		const unsubs: Array<Promise<() => void>> = [
			listen('pkg-installed', () => void refresh()),
			listen('pkg-uninstalled', () => void refresh()),
			listen('pkg-reloaded', () => void refresh()),
			// WP-11: a pkg pushed/cleared its rail badge via
			// `pkg_activity_bar_set_badge` — refetch the kernel snapshot rather
			// than patching in place so this stays a single source of truth.
			listen('pkg-badge-changed', () => void refresh()),
		];
		return () => {
			cancelled = true;
			for (const p of unsubs) void p.then((fn) => fn());
		};
	}, []);

	return { entries, loaded };
}
