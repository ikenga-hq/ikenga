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
// `pin_on_install` (§3): honoured ONCE at first install — the kernel emits
// `pkg-installed` only for a fresh `pkg_installed` row (reinstalls emit
// `pkg-reloaded`, boot replay emits nothing), so the event itself is the
// freshness signal. We dedupe against existing pins on `manifestId` + target
// so an update or a second hook mount can never double-pin.

import { listen } from '@/lib/transport';
import { useEffect, useState } from 'react';
import { activityPinsAdd, activityPinsList, pkgKernelStatus } from '@/lib/tauri-cmd';

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
}

interface SidecarStatus {
	pkg_id: string;
	state: string;
	last_err?: string | null;
}

/** Kernel `pkg-installed` event payload (emitted by `Kernel::install_from_path`
 *  on fresh installs only — never on reinstall or boot replay). */
interface PkgInstalledEvent {
	pkg_id: string;
	version: string;
	installed_at: number;
}

/** Pkg ids with a `pin_on_install` application currently in flight. The hook
 *  is mounted in several places (activity bar, Views section, section
 *  registry), so the same `pkg-installed` event arrives at N listeners —
 *  first claim wins, the rest no-op. Reuses are harmless: the
 *  manifestId+target dedupe inside `applyPinOnInstall` is the durable guard. */
const pinOnInstallInflight = new Set<string>();

/** Apply `pin_on_install` for a freshly-installed pkg: for every view that
 *  declares it, add a `route` pin unless a pin with the same `manifestId`
 *  (`pkg_id`) + `target` already exists (G-MANIFEST-V5 §3). Best-effort —
 *  never throws. */
async function applyPinOnInstall(pkgId: string): Promise<void> {
	if (pinOnInstallInflight.has(pkgId)) return;
	pinOnInstallInflight.add(pkgId);
	try {
		const [status, pins] = await Promise.all([pkgKernelStatus(), activityPinsList()]);
		const reg = (status.registries.views ?? {}) as { entries?: PkgViewEntry[] };
		const targets = (reg.entries ?? []).filter((v) => v.pkg_id === pkgId && v.pin_on_install);
		for (const view of targets) {
			const alreadyPinned = pins.some(
				(p) =>
					(p.manifestId === pkgId && p.target === view.pane_route) ||
					// Belt-and-braces: a user-created route pin at the same target
					// counts as covered even if its manifestId differs.
					(p.kind === 'route' && p.target === view.pane_route)
			);
			if (alreadyPinned) continue;
			await activityPinsAdd({
				kind: 'route',
				target: view.pane_route,
				label: view.title,
				iconLucide: view.icon ?? null,
				sectionId: null,
				manifestId: pkgId,
			});
		}
	} catch (err) {
		console.warn(`[pkg-views] pin_on_install for ${pkgId} failed:`, err);
	} finally {
		pinOnInstallInflight.delete(pkgId);
	}
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

export function usePkgActivityBarEntries(): PkgActivityBarState {
	const [entries, setEntries] = useState<PkgActivityBarEntry[]>([]);
	const [views, setViews] = useState<PkgViewEntry[]>([]);
	const [loaded, setLoaded] = useState(false);

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
				}
			} catch {
				if (!cancelled) {
					setViews([]);
					setEntries([]);
				}
			} finally {
				if (!cancelled) setLoaded(true);
			}
		}

		void refresh();

		// Kernel lifecycle events. The names match those emitted by the pkg
		// kernel in `kernel.rs` and `commands/pkg_dev.rs`.
		const unsubs: Array<Promise<() => void>> = [
			listen<PkgInstalledEvent>('pkg-installed', (ev) => {
				// G-MANIFEST-V5 §3: `pin_on_install` applies only on a fresh
				// install — the kernel emits this event only for a new
				// `pkg_installed` row, so the event itself is the freshness
				// check. Pin first, then refresh so the new pin and the new
				// registry state land together.
				void applyPinOnInstall(ev.payload.pkg_id).then(() => refresh());
			}),
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

	return { entries, views, loaded };
}
