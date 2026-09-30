// Activity-bar pin store — wraps the Tauri commands in a Zustand store and
// keeps the in-memory list in sync with SQLite. Mutations are optimistic:
// the local snapshot updates first, then the Rust call confirms (or errors
// and we re-hydrate from disk). Sections are kept in a separate slice so a
// pin update doesn't invalidate the sections list and vice versa.
//
// Section creation flow lives at the call sites — the store just exposes
// `createSection` and `addPin`. The "fuzzy match → prompt to create"
// dialog is UI; the host is the source of truth for what exists.

import { useMemo } from 'react';
import { create } from 'zustand';
import { listen } from '@/lib/transport';
import {
	activityPinsAdd,
	activityPinsList,
	activityPinsRemove,
	activityPinsReorder,
	activitySectionsCreate,
	activitySectionsList,
	activitySectionsRemove,
	activitySectionsUpdate,
	type ActivityPin,
	type ActivityPinKind,
	type ActivitySection,
} from '@/lib/tauri-cmd';

export type Pin = ActivityPin;
export type Section = ActivitySection;
export type PinKind = ActivityPinKind;

/** Reserved ids — host-owned, cannot be created by users. Mirrors the
 *  validation on the Rust side. Exposed so UI can guide users away from
 *  those names before sending the create call. */
export const RESERVED_SECTION_IDS: readonly string[] = Object.freeze(['system', 'settings']);

export interface PinsState {
	pins: Pin[];
	sections: Section[];
	hydrated: boolean;
	loading: boolean;
	error: string | null;

	hydrate: () => Promise<void>;
	refresh: () => Promise<void>;

	/** Add a pin. Throws if `sectionId` references a missing section —
	 *  callers should create the section first via `createSection` (or
	 *  pass `null` for section-less pins). Pass `manifestId` for artifact
	 *  pins so `ikenga://artifact/<id>` resolves back to this pin; the host
	 *  rejects duplicates with a clear error. */
	addPin: (args: {
		kind: PinKind;
		target: string;
		label: string;
		iconLucide?: string | null;
		iconEmoji?: string | null;
		sectionId?: string | null;
		manifestId?: string | null;
	}) => Promise<Pin>;

	removePin: (id: string) => Promise<void>;

	/** Reorder pins within a single section. Pass empty string for the
	 *  section-less group. */
	reorderPins: (orderedIds: string[], sectionId: string) => Promise<void>;

	createSection: (args: {
		id: string;
		label: string;
		iconLucide?: string | null;
		iconEmoji?: string | null;
	}) => Promise<Section>;

	updateSection: (args: {
		id: string;
		label?: string;
		iconLucide?: string | null;
		iconEmoji?: string | null;
	}) => Promise<Section>;

	removeSection: (id: string) => Promise<void>;
}

/** Slugify a free-form label into a section id ([a-z0-9_-]). The host's
 *  `validate_section_id` is the source of truth for what's accepted; this
 *  helper just produces a candidate id from a label so callers don't have
 *  to invent their own normalization. */
export function slugifySectionId(input: string): string {
	return input
		.toLowerCase()
		.trim()
		.replace(/[^a-z0-9_-]+/g, '-')
		.replace(/^-+|-+$/g, '');
}

/** Fuzzy-match an existing section by id or label. Returns the matched
 *  section if any name contains, or is contained by, the candidate
 *  (case-insensitive). Used at pin time so the UI can ask "did you mean
 *  Finance?" before prompting to create a new section. */
export function fuzzyMatchSection(candidate: string, sections: readonly Section[]): Section | null {
	if (!candidate.trim()) return null;
	const slug = slugifySectionId(candidate);
	// 1. Exact id hit.
	const exact = sections.find((s) => s.id === slug);
	if (exact) return exact;
	// 2. Substring on label or id.
	const lc = candidate.toLowerCase();
	for (const s of sections) {
		const labelLc = s.label.toLowerCase();
		if (labelLc === lc) return s;
		if (labelLc.includes(lc) || lc.includes(labelLc)) return s;
		if (s.id.includes(slug) || slug.includes(s.id)) return s;
	}
	return null;
}

export const usePinsStore = create<PinsState>((set, get) => ({
	pins: [],
	sections: [],
	hydrated: false,
	loading: false,
	error: null,

	hydrate: async () => {
		if (get().hydrated || get().loading) return;
		set({ loading: true, error: null });
		try {
			const [pins, sections] = await Promise.all([activityPinsList(), activitySectionsList()]);
			set({ pins, sections, hydrated: true, loading: false });
		} catch (e) {
			set({ error: String(e), loading: false });
		}
	},

	refresh: async () => {
		set({ loading: true, error: null });
		try {
			const [pins, sections] = await Promise.all([activityPinsList(), activitySectionsList()]);
			set({ pins, sections, hydrated: true, loading: false });
		} catch (e) {
			set({ error: String(e), loading: false });
		}
	},

	addPin: async (args) => {
		const pin = await activityPinsAdd(args);
		set({ pins: [...get().pins, pin] });
		return pin;
	},

	removePin: async (id) => {
		// Optimistic remove; on error refresh from disk.
		const before = get().pins;
		set({ pins: before.filter((p) => p.id !== id) });
		try {
			await activityPinsRemove(id);
		} catch (e) {
			set({ pins: before, error: String(e) });
			throw e;
		}
	},

	reorderPins: async (orderedIds, sectionId) => {
		// Optimistic reorder of the slice belonging to this section.
		const before = get().pins;
		const inSection = (p: Pin) =>
			sectionId === '' ? p.sectionId === null : p.sectionId === sectionId;
		const others = before.filter((p) => !inSection(p));
		const idMap = new Map(before.map((p) => [p.id, p] as const));
		const reordered: Pin[] = [];
		orderedIds.forEach((id, idx) => {
			const p = idMap.get(id);
			if (p) {
				reordered.push({
					...p,
					sortOrder: idx,
					sectionId: sectionId === '' ? null : sectionId,
				});
			}
		});
		set({ pins: [...others, ...reordered] });
		try {
			await activityPinsReorder(orderedIds, sectionId);
		} catch (e) {
			set({ pins: before, error: String(e) });
			throw e;
		}
	},

	createSection: async (args) => {
		const section = await activitySectionsCreate(args);
		set({ sections: [...get().sections, section] });
		return section;
	},

	updateSection: async (args) => {
		const section = await activitySectionsUpdate(args);
		set({
			sections: get().sections.map((s) => (s.id === section.id ? section : s)),
		});
		return section;
	},

	removeSection: async (id) => {
		// Optimistic delete; the SQL ON DELETE SET NULL re-parents pins to NULL.
		const before = { sections: get().sections, pins: get().pins };
		set({
			sections: before.sections.filter((s) => s.id !== id),
			pins: before.pins.map((p) => (p.sectionId === id ? { ...p, sectionId: null } : p)),
		});
		try {
			await activitySectionsRemove(id);
		} catch (e) {
			set({
				sections: before.sections,
				pins: before.pins,
				error: String(e),
			});
			throw e;
		}
	},
}));

/** Minimal pane-store surface this module needs to dispatch a pin click.
 *  Defined here (instead of importing the real store) so tests can pass
 *  in a shape-only stub without touching pane internals. */
export interface PinDispatchTarget {
	focusedId: string;
	navigateFocused: (path: string) => void;
	placeView: (
		dstLeafId: string,
		view: { kind: 'artifact'; path: string },
		mode: 'append'
	) => boolean;
}

/** Dispatch a pin selection to the pane store. Route + pkg-route navigate
 *  the focused leaf; artifact / file / external place the path as an
 *  artifact view (the auto-router picks the renderer by MIME at mount).
 *  Pure with respect to the inputs — extracted from the ActivityBar so it
 *  can be unit-tested without rendering. */
export function dispatchPinSelection(pin: Pin, store: PinDispatchTarget): void {
	if (pin.kind === 'route' || pin.kind === 'pkg-route') {
		store.navigateFocused(pin.target);
		return;
	}
	store.placeView(store.focusedId, { kind: 'artifact', path: pin.target }, 'append');
}

/** Selector hook returning a stable Map<path, manifestId> built from the
 *  current pin set. Used by `formatPaneAddressForDisplay` so the URL bar
 *  can swap a file path for its canonical `ikenga://artifact/<id>` URI on
 *  pinned artifacts. Memoized on the pins-array reference so renders that
 *  don't touch pins (the common case) don't allocate a new Map. */
export function usePathToManifestId(): ReadonlyMap<string, string> {
	const pins = usePinsStore((s) => s.pins);
	return useMemo(() => {
		const map = new Map<string, string>();
		for (const pin of pins) {
			if (pin.manifestId) map.set(pin.target, pin.manifestId);
		}
		return map;
	}, [pins]);
}

/** Compute the desired ordered id list for a same-section drag-and-drop
 *  reorder. `pins` is the section's current order, `srcIdx` is the dragged
 *  pin's index, `dstIdx` is the index it should land at *as if the source
 *  were removed first*. Returns an empty array if the inputs are invalid;
 *  callers should treat that as "no-op, don't post to the host".
 *
 *  Extracted from the settings page so the off-by-one math is unit-tested
 *  once instead of being argued about per call site. */
export function computeReorderIds<T extends { id: string }>(
	pins: readonly T[],
	srcIdx: number,
	dstIdx: number
): string[] {
	if (srcIdx < 0 || srcIdx >= pins.length) return [];
	const src = pins[srcIdx];
	if (!src) return [];
	const ids = pins.filter((_, i) => i !== srcIdx).map((p) => p.id);
	const insertAt = Math.min(Math.max(dstIdx, 0), ids.length);
	ids.splice(insertAt, 0, src.id);
	return ids;
}

/** Compute the desired ordered id list for a cross-section drop. `pins` is
 *  the *destination* section's current order, `pinId` is the moved pin,
 *  `dstIdx` is where it should land. */
export function computeCrossSectionReorderIds<T extends { id: string }>(
	pins: readonly T[],
	pinId: string,
	dstIdx: number
): string[] {
	const ids = pins.map((p) => p.id);
	const insertAt = Math.min(Math.max(dstIdx, 0), ids.length);
	ids.splice(insertAt, 0, pinId);
	return ids;
}

/** The pkg a route / pkg-route pin opens (`/pkg/<id>`, `/pkg/<id>/…`), or
 *  null for any other pin. */
export function pkgIdOfPin(pin: Pick<Pin, 'kind' | 'target'>): string | null {
	if (pin.kind !== 'route' && pin.kind !== 'pkg-route') return null;
	const m = /^\/pkg\/([^/?#]+)/.exec(pin.target);
	if (!m) return null;
	try {
		return decodeURIComponent(m[1]!);
	} catch {
		return m[1]!;
	}
}

/** Drop pins into a pkg the kernel does not currently have (on disk but
 *  failed to register, say): they would open "No such package route".
 *  Display-only — the pins stay stored, so they come back as soon as the
 *  pkg registers. `available` null = not known yet: keep everything. */
export function visiblePins<T extends Pick<Pin, 'kind' | 'target'>>(
	pins: readonly T[],
	available: ReadonlySet<string> | null | undefined
): T[] {
	if (!available) return [...pins];
	return pins.filter((p) => {
		const pkgId = pkgIdOfPin(p);
		return pkgId === null || available.has(pkgId);
	});
}

/** Selector hook returning pins grouped by section, plus a loose group for
 *  section-less pins. Consumers iterate `sections` to render headers in
 *  user order, then look up `pinsBySection.get(section.id)`.
 *
 *  `availablePkgIds` (the kernel's current pkgs, from
 *  `usePkgActivityBarEntries`) hides pins into a pkg that is not registered;
 *  see `visiblePins`. */
export function useActivityBarPins(availablePkgIds?: ReadonlySet<string> | null) {
	const pins = usePinsStore((s) => s.pins);
	const sections = usePinsStore((s) => s.sections);
	const hydrated = usePinsStore((s) => s.hydrated);

	const sortedSections = [...sections].sort(
		(a, b) => a.sortOrder - b.sortOrder || a.createdAt.localeCompare(b.createdAt)
	);
	const sortedPins = visiblePins(pins, availablePkgIds).sort(
		(a, b) => a.sortOrder - b.sortOrder || a.createdAt.localeCompare(b.createdAt)
	);
	const pinsBySection = new Map<string, Pin[]>();
	const sectionLessPins: Pin[] = [];
	for (const pin of sortedPins) {
		if (pin.sectionId === null) {
			sectionLessPins.push(pin);
		} else {
			const list = pinsBySection.get(pin.sectionId) ?? [];
			list.push(pin);
			pinsBySection.set(pin.sectionId, list);
		}
	}
	return {
		sections: sortedSections,
		pinsBySection,
		sectionLessPins,
		hydrated,
	};
}

/** The rail pins that point into pkg `pkgId`: route / pkg-route pins whose
 *  target is one of its pane paths (`/pkg/<id>` or under `/pkg/<id>/…`), or
 *  that `pin_on_install` created for it (`manifestId === pkgId`). Artifact /
 *  file / external pins are never matched — their `manifestId` is an artifact
 *  id, not a pkg id. */
export function pinsOfPkg<T extends Pick<Pin, 'kind' | 'target' | 'manifestId'>>(
	pins: readonly T[],
	pkgId: string
): T[] {
	const root = `/pkg/${pkgId}`;
	return pins.filter((p) => {
		if (p.kind !== 'route' && p.kind !== 'pkg-route') return false;
		if (p.manifestId === pkgId) return true;
		return (
			p.target === root ||
			p.target.startsWith(`${root}/`) ||
			p.target.startsWith(`${root}?`) ||
			p.target.startsWith(`${root}#`)
		);
	});
}

/** An uninstalled pkg's views are gone, so its rail pins would open nothing:
 *  delete them (read from disk, not the in-memory slice, so a pin added before
 *  the rail hydrated is caught too), then re-read the store. Best-effort —
 *  one failed removal does not stop the rest. Returns the removed pin ids. */
export async function prunePinsForUninstalledPkg(pkgId: string): Promise<string[]> {
	let dead: Pin[];
	try {
		dead = pinsOfPkg(await activityPinsList(), pkgId);
	} catch (err) {
		console.warn(`[pins] reading pins to prune for ${pkgId} failed:`, err);
		return [];
	}
	const removed: string[] = [];
	for (const pin of dead) {
		try {
			await activityPinsRemove(pin.id);
			removed.push(pin.id);
		} catch (err) {
			console.warn(`[pins] removing pin ${pin.id} of uninstalled ${pkgId} failed:`, err);
		}
	}
	if (removed.length > 0) await usePinsStore.getState().refresh();
	return removed;
}

/** Subscribe to the kernel's `pkg-uninstalled` event and prune that pkg's
 *  pins. Mount once (the rail does). Returns the unsubscribe. */
export function subscribePinPruneOnUninstall(): () => void {
	// Caught at once: with no event transport (tests, a browser without the
	// bridge) `listen` rejects, and that must not surface as an unhandled
	// rejection long before the rail unmounts.
	const unlisten = listen<{ pkg_id: string }>('pkg-uninstalled', (ev) => {
		const id = ev.payload?.pkg_id;
		if (id) void prunePinsForUninstalledPkg(id);
	}).catch((err) => {
		console.warn('[pins] pkg-uninstalled subscription failed:', err);
		return () => {};
	});
	return () => void unlisten.then((fn) => fn());
}
