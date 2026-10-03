// Activity-bar entries contributed by installed pkgs via manifest `ui.views[0]`
// (manifest v5, G-MANIFEST-V5 §2). The `ui.nav` alias window closed with
// DEC-37, so `ui.views[]` is the only source: both the `views` registry and
// the `activity_bar` registry snapshot are views-sourced on the Rust side.
//
// Read from the pkg kernel snapshot and re-fetched on pkg install / uninstall /
// reload so newly-mounted pkgs appear (and removed ones disappear) without a
// shell restart.
//
// Shared by `activity-bar.tsx` (renders one rail icon per entry / merges
// badge+parked state into pins) and `explorer/sections/views.tsx` (renders the
// full `views` list). The `loaded` flag lets callers distinguish "no pkgs
// installed" from "snapshot not fetched yet" — the activity bar needs that to
// avoid reconciling a persisted pkg mode before the kernel snapshot arrives.
//
// `pin_on_install` (§3) is applied by the kernel itself, in the same call that
// writes a fresh `pkg_installed` row (`pkg/pin_on_install.rs`), before it emits
// `pkg-installed`. It used to run here and never pinned a reverse-DNS pkg id:
// the pin store rejects a dotted `manifestId`, and the rejection was swallowed.
// Doing it in the kernel also means it no longer depends on this hook being
// mounted when the event fires. This hook only refreshes on the event.

import { listen } from '@/lib/transport';
import { useEffect, useState } from 'react';
import { pkgKernelStatus } from '@/lib/tauri-cmd';

/** Shape mirrors the Rust `ActivityBarBadge` in
 *  `pkg/registries/activity_bar.rs` (WP-11). */
export interface PkgActivityBarBadge {
	dot: boolean;
	count?: number | null;
	tooltip?: string | null;
}

/** The legacy `NavEntry` wire shape that `ui.views[]` entries are mapped onto
 *  in the activity-bar registry. Mirrors `pkg::manifest::NavEntry` in Rust.
 *  The name is historical — the `ui.nav` manifest field is gone (DEC-37), but
 *  the wire shape stays: the pkg-mode sidebar and the WP-22 pin seed
 *  (`lib/shell/seed-pins.ts`) both read it. */
export interface PkgNavEntry {
	id: string;
	label: string;
	icon?: string | null;
	section?: string | null;
	route: string;
}

/** One `ui.views[]` contribution as surfaced by the `views` kernel registry.
 *  Mirrors `ViewRegistryEntry` in `pkg/registries/views.rs` (WP-28). */
export interface PkgViewEntry {
	pkg_id: string;
	pkg_name: string;
	/** `${pkg_id}:${view_id}` — the namespaced identity. */
	qualified_id: string;
	/** The pkg-local view id. */
	id: string;
	title: string;
	icon?: string | null;
	/** Manifest-declared namespace path — equals a `ui.routes[].path`. */
	route: string;
	/** `/pkg/<id><route>` — the pane path the pane store / pins navigate. */
	pane_route: string;
	pin_on_install: boolean;
}

/** Shape mirrors the Rust `ActivityBarEntry` in
 *  `pkg/registries/activity_bar.rs`. */
export interface PkgActivityBarEntry {
	pkg_id: string;
	pkg_name: string;
	id: string;
	/** Rail label: the package display name. */
	label: string;
	icon?: string | null;
	section?: string | null;
	/** Pane path of `views[0]` — `/pkg/<id><route>`. */
	route: string;
	/** All of the pkg's views mapped onto the NavEntry wire shape — the
	 *  pkg-mode sidebar menu / pin-label source. */
	nav: PkgNavEntry[];
	badge?: PkgActivityBarBadge | null;
	/** True when the sidecar supervisor reports this pkg as `parked`. */
	parked?: boolean;
	/** Sidecar `last_err` when parked. */
	parked_reason?: string | null;
}

export interface PkgActivityBarState {
	/** Rail claims — `views[0]` per pkg (plus the registry fallback below). */
	entries: PkgActivityBarEntry[];
	/** Every contributed view across installed pkgs — the Explorer **Views**
	 *  section's list. */
	views: PkgViewEntry[];
	/** True once the first kernel-snapshot fetch has resolved (success or
	 *  failure). Until then `entries`/`views` are empty placeholders, not a
	 *  real "nothing installed" answer. */
	loaded: boolean;
	/** Pkg ids the kernel currently has: any registered view / rail entry /
	 *  UI route, or an install record (a pkg parked for capability review has
	 *  no views but its route shows the consent step). A rail pin into a pkg
	 *  outside this set would open "No such package route", so the rail hides
	 *  it — without deleting it, so it returns once the pkg registers again.
	 *  `null` until the first snapshot, or when the snapshot failed: unknown,
	 *  so nothing is hidden. */
	availablePkgIds: ReadonlySet<string> | null;
}

interface SidecarStatus {
	pkg_id: string;
	state: string;
	last_err?: string | null;
}

/** Build the `entries` rail claims: `views[0]` per pkg from the views
 *  registry, with the `activity_bar` registry filling in any pkg missing from
 *  it. Both registries are views-sourced post-DEC-37, so that fallback only
 *  fires if the two registries disagree (e.g. `ViewsRegistry::register`
 *  rejected a §2b route reference the activity bar accepted). Badges merge from `activity_bar` entries (the badge lives on that
 *  registry entry per WP-11); `parked` merges from the sidecar supervisor. */
function mergeRegistries(
	views: PkgViewEntry[],
	activityBar: PkgActivityBarEntry[],
	parkedByPkg: Map<string, SidecarStatus>
): PkgActivityBarEntry[] {
	const activityBarByPkg = new Map(activityBar.map((e) => [e.pkg_id, e]));
	const byPkg = new Map<string, PkgViewEntry[]>();
	for (const v of views) {
		const list = byPkg.get(v.pkg_id) ?? [];
		list.push(v);
		byPkg.set(v.pkg_id, list);
	}

	const out: PkgActivityBarEntry[] = [];
	for (const [pkgId, pkgViews] of byPkg) {
		const first = pkgViews[0]!;
		const legacy = activityBarByPkg.get(pkgId);
		out.push({
			pkg_id: pkgId,
			pkg_name: first.pkg_name,
			id: first.id,
			label: first.pkg_name,
			icon: first.icon ?? null,
			section: null,
			route: first.pane_route,
			nav: pkgViews.map((v) => ({
				id: v.id,
				label: v.title,
				icon: v.icon ?? null,
				section: null,
				route: v.pane_route,
			})),
			badge: legacy?.badge ?? null,
		});
	}
	// Registry-disagreement fallback: pkgs present in `activity_bar` but
	// missing from `views`. Belt-and-braces — see mergeRegistries' doc.
	for (const e of activityBar) {
		if (!byPkg.has(e.pkg_id)) out.push(e);
	}

	return out.map((e) => {
		const sidecar = parkedByPkg.get(e.pkg_id);
		if (sidecar?.state === 'parked') {
			return { ...e, parked: true, parked_reason: sidecar.last_err ?? null };
		}
		return e;
	});
}

/** Pkg ids with anything registered in the kernel snapshot, or installed.
 *  See `PkgActivityBarState.availablePkgIds`. */
export function availablePkgIdsOf(status: {
	installed?: ReadonlyArray<{ id: string }>;
	registries?: Record<string, unknown>;
}): Set<string> {
	const ids = new Set<string>();
	for (const i of status.installed ?? []) ids.add(i.id);
	for (const name of ['views', 'activity_bar', 'ui_routes']) {
		const reg = status.registries?.[name] as { entries?: unknown } | undefined;
		if (!Array.isArray(reg?.entries)) continue;
		for (const e of reg.entries as Array<{ pkg_id?: unknown }>) {
			if (typeof e?.pkg_id === 'string') ids.add(e.pkg_id);
		}
	}
	return ids;
}

export function usePkgActivityBarEntries(): PkgActivityBarState {
	const [entries, setEntries] = useState<PkgActivityBarEntry[]>([]);
	const [views, setViews] = useState<PkgViewEntry[]>([]);
	const [loaded, setLoaded] = useState(false);
	const [availablePkgIds, setAvailablePkgIds] = useState<ReadonlySet<string> | null>(null);

	useEffect(() => {
		let cancelled = false;

		async function refresh() {
			try {
				const status = await pkgKernelStatus();
				const viewsReg = (status.registries.views ?? {}) as {
					entries?: PkgViewEntry[];
				};
				const activityBar = (status.registries.activity_bar ?? {}) as {
					entries?: PkgActivityBarEntry[];
				};
				const supervisor = (status.registries.sidecar_supervisor ?? {}) as {
					entries?: SidecarStatus[];
				};
				const parkedByPkg = new Map<string, SidecarStatus>();
				for (const s of supervisor.entries ?? []) {
					parkedByPkg.set(s.pkg_id, s);
				}
				const viewEntries = viewsReg.entries ?? [];
				const merged = mergeRegistries(viewEntries, activityBar.entries ?? [], parkedByPkg);
				if (!cancelled) {
					setViews(viewEntries);
					setEntries(merged);
					setAvailablePkgIds(availablePkgIdsOf(status));
				}
			} catch {
				if (!cancelled) {
					setViews([]);
					setEntries([]);
					setAvailablePkgIds(null);
				}
			} finally {
				if (!cancelled) setLoaded(true);
			}
		}

		void refresh();

		// Kernel lifecycle events. The names match those emitted by the pkg
		// kernel in `kernel.rs` and `commands/pkg_dev.rs`.
		const unsubs: Array<Promise<() => void>> = [
			// A fresh install: the kernel has already written any
			// `pin_on_install` pins (the pins store refreshes on this event too).
			listen('pkg-installed', () => void refresh()),
			listen('pkg-uninstalled', () => void refresh()),
			listen('pkg-reloaded', () => void refresh()),
			// WP-11: a pkg pushed/cleared its rail badge via
			// `pkg_activity_bar_set_badge` — refetch the kernel snapshot rather
			// than patching in place so this stays a single source of truth.
			listen('pkg-badge-changed', () => void refresh()),
			// Sidecar state changes (parked, crashed, running) also affect how
			// the rail entry is rendered.
			listen('pkg://lifecycle', () => void refresh()),
		];
		return () => {
			cancelled = true;
			for (const p of unsubs) void p.then((fn) => fn());
		};
	}, []);

	return { entries, views, loaded, availablePkgIds };
}
