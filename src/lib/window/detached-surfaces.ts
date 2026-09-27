// Detached-surface tracking for the PRIMARY window (plans/multi-window).
//
// When a pane is popped out (`spawnWindow({ surface_set: ["terminal:<id>"] })`),
// its surface is now live in a separate thin window. Without this tracker the
// primary window keeps rendering the same surface too, so the terminal /
// viewer shows up DUPLICATED in both windows (and, for terminal, two
// live views drive the same shared Rust-core session).
//
// The source of truth is the Rust `WindowRegistry`: every spawned window's
// descriptor (with its `surface_set`) is listed by `listWindows()`, and the
// registry broadcasts `window://opened` / `window://closed` whenever the set
// changes. This store seeds from `listWindows()` and re-syncs on each lifecycle
// event, exposing `surfaceId -> hosting window label`. A pane view consults
// `useIsSurfaceDetached(surfaceId)` and, when true, renders a "popped out"
// placeholder instead of the live duplicate.
//
// PRIMARY-WINDOW ONLY. A thin detached window never renders the
// pop-out-able pane views, so it has nothing to track; the initializer
// no-ops there.
//
// WP-69 (G-SEATS §4.4, DEC-69d): a detached window now holds SEVERAL surfaces
// as tabs — Pop out joins "Window 2" instead of always spawning. The map was
// already `surfaceId → label`, so many surfaces can point at one label; what
// changes here:
//   • `window://surfaces-changed` (a join or a move back) is applied straight
//     from its payload (`applySurfacesChanged`), no re-list;
//   • `reclaimSurface` takes ONE surface out of its window
//     (`windowRemoveSurface`) instead of closing the whole window — Rust
//     closes the window only when that was its last surface;
//   • surfaces that come back to the main window (a Move back, or their
//     window closing) are announced to `onSurfacesReturned` listeners, so the
//     Companion can mount them and say so (D-09 `moveBack` / `closeWin2`).

import { WINDOW_TOPICS } from '@ikenga/contract';
import { listen } from '@/lib/transport';
import { isTauri } from '@/lib/transport';
import { create } from 'zustand';

import { listWindows, windowRemoveSurface } from '@/lib/tauri-cmd';
import {
	applySurfacesChanged,
	MAKE_TARGET_TOPIC,
	type MakeTargetRequest,
	PENDING_WINDOW_LABEL,
	returnedByClosedWindows,
	SURFACES_CHANGED_TOPIC,
	type SurfacesChangedEnvelope,
	type SurfacesChangedPayload,
	surfacesOf,
} from './surfaces-topic';
import { isDetachedWindow } from './window-context';

interface DetachedSurfacesState {
	/** `surfaceId` (e.g. `"terminal:<ptyId>"`) → label of the window hosting it.
	 *  Several surfaces may share a label: that window shows them as tabs. */
	surfaceToWindow: Record<string, string>;
}

/** Why surfaces came back to the main window. */
export interface SurfacesReturned {
	/** The detached window they left. */
	label: string;
	surfaceIds: string[];
	/** `move-back`: Window 2 ⋯ → Move back to main window (one surface).
	 *  `window-closed`: the window closed with them still in it. */
	reason: 'move-back' | 'window-closed';
}

const returnListeners = new Set<(e: SurfacesReturned) => void>();
const makeTargetListeners = new Set<(surfaceId: string) => void>();

/** Subscribe to Window 2 ⋯ → *Make dispatch target* requests (primary only).
 *  Returns an unsubscribe. */
export function onMakeTargetRequested(cb: (surfaceId: string) => void): () => void {
	makeTargetListeners.add(cb);
	return () => {
		makeTargetListeners.delete(cb);
	};
}

/** Deliver one *Make dispatch target* request. Exported for tests. */
export function handleMakeTargetRequest(req: Partial<MakeTargetRequest> | null | undefined): void {
	const surfaceId = req?.surfaceId;
	if (typeof surfaceId !== 'string' || !surfaceId) return;
	for (const cb of makeTargetListeners) {
		try {
			cb(surfaceId);
		} catch (err) {
			console.warn('detached-surfaces: make-target listener failed', err);
		}
	}
}

/** Subscribe to surfaces returning to the main window. Returns an unsubscribe. */
export function onSurfacesReturned(cb: (e: SurfacesReturned) => void): () => void {
	returnListeners.add(cb);
	return () => {
		returnListeners.delete(cb);
	};
}

function announceReturned(e: SurfacesReturned): void {
	if (e.surfaceIds.length === 0) return;
	for (const cb of returnListeners) {
		try {
			cb(e);
		} catch (err) {
			console.warn('detached-surfaces: return listener failed', err);
		}
	}
}

export const useDetachedSurfaces = create<DetachedSurfacesState>(() => ({
	surfaceToWindow: {},
}));

/**
 * T-3a (reclaim half of T-2, plans/multi-window "corruption 2 (reflow)"):
 * surface ids that just transitioned detached → not-detached (a reclaim),
 * armed here so a pane view can fire ONE T-2-style repaint nudge on the
 * XTermHost mount the reclaim produces, instead of the missing-SIGWINCH bug
 * T-2 left on this side of the round trip. `surfaceToWindow` above is
 * level-only (a snapshot of "who's detached right now") — it carries no
 * transition marker on its own, so this module-scope set is the signal.
 * Populated from BOTH `reclaimSurface()` (the "Bring it back" button) and
 * the diff inside `syncDetachedSurfaces()` (the `window://closed` re-sync,
 * which also covers the titlebar-close path — closing the OS window
 * directly, without going through `reclaimSurface()`).
 *
 * Consumers should `hasPendingReclaimNudge` (pure peek, safe during React
 * render) and `clearPendingReclaimNudge` (idempotent mutation, call from an
 * effect so a StrictMode double-invoke can't lose the read) rather than
 * touching this set directly.
 */
const pendingReclaimNudge = new Set<string>();

/** Pure peek — does NOT mutate. Safe to call during render, including React
 *  StrictMode's double-invoked render pass. */
export function hasPendingReclaimNudge(surfaceId: string): boolean {
	return pendingReclaimNudge.has(surfaceId);
}

/** Consume the flag. Idempotent (a second call is a harmless no-op), so it's
 *  safe under React StrictMode's double-invoked effects — call this from an
 *  effect, never from render, since render must stay a pure read. */
export function clearPendingReclaimNudge(surfaceId: string): void {
	pendingReclaimNudge.delete(surfaceId);
}

/**
 * Rebuild the surface→window map from the authoritative registry list. Exported
 * as the reconcile path — e.g. after a `spawnWindow` rejection un-does an
 * optimistic `markSurfaceDetached`, since the failed window never lands in the
 * registry list.
 */
export async function syncDetachedSurfaces(): Promise<void> {
	try {
		const windows = await listWindows();
		const map: Record<string, string> = {};
		for (const w of windows) {
			if (w.label === 'main') continue;
			for (const surfaceId of w.surface_set) map[surfaceId] = w.label;
		}
		// A Pop out still resolving (join or spawn in flight) isn't in the
		// registry yet; keep its provisional entry so the origin pane doesn't
		// flash the live duplicate. `popOutSurface` clears it on failure.
		for (const [surfaceId, label] of Object.entries(useDetachedSurfaces.getState().surfaceToWindow)) {
			if (label === PENDING_WINDOW_LABEL && !(surfaceId in map)) map[surfaceId] = label;
		}
		// T-3a: any surface that was detached a moment ago and no longer is —
		// including a reclaim that happened via the OS titlebar close rather
		// than the in-app "Bring it back" button — just got reclaimed. Arm the
		// nudge here too (not just in `reclaimSurface()` below) so that path is
		// covered.
		const prev = useDetachedSurfaces.getState().surfaceToWindow;
		for (const surfaceId of Object.keys(prev)) {
			// Only terminal surfaces are armed: `TerminalView` is the sole
			// consumer, and therefore the sole caller of
			// `clearPendingReclaimNudge`. Arming a `viewer` surface here
			// would add an entry nothing ever clears, leaking one Set slot per
			// non-terminal pop-out for the life of the session.
			if (!(surfaceId in map) && surfaceId.startsWith('terminal:')) {
				pendingReclaimNudge.add(surfaceId);
			}
		}
		useDetachedSurfaces.setState({ surfaceToWindow: map });
		// WP-69: surfaces whose window closed with them still in it came back
		// to the main window (D-09 `closeWin2`).
		const live = new Set(windows.map((w) => w.label));
		for (const [label, surfaceIds] of Object.entries(returnedByClosedWindows(prev, map, live))) {
			announceReturned({ label, surfaceIds, reason: 'window-closed' });
		}
	} catch (e) {
		console.warn('detached-surfaces: refresh failed', e);
	}
}

/**
 * WP-69: apply a `window://surfaces-changed` payload — a join (Pop out into
 * Window 2) or a removal (Move back / a reclaim). Exported for tests.
 */
export function handleSurfacesChanged(change: SurfacesChangedPayload): void {
	const prev = useDetachedSurfaces.getState().surfaceToWindow;
	const next = applySurfacesChanged(prev, change);
	const back = change.removed.filter((surfaceId) => !(surfaceId in next));
	// T-3a: a terminal leaving its window re-mounts inline — arm its nudge,
	// as the close-driven re-sync above does. Only when it was detached a
	// moment ago (an optimistic reclaim already armed and cleared it).
	for (const surfaceId of back) {
		if (surfaceId in prev && surfaceId.startsWith('terminal:')) pendingReclaimNudge.add(surfaceId);
	}
	useDetachedSurfaces.setState({ surfaceToWindow: next });
	if (change.move_back) announceReturned({ label: change.label, surfaceIds: back, reason: 'move-back' });
}

let started = false;

/**
 * Seed the tracker and subscribe to the `window://` lifecycle bus. Idempotent;
 * call once from the primary-window bootstrap. No-ops in a detached window.
 *
 * The opened/closed envelopes carry only `{ label, … }`, not the full
 * descriptor, so each event triggers a cheap `listWindows()` re-sync rather
 * than trying to mutate the map from the payload. The primary window may miss
 * its own siblings' very first `opened` event in a race; the initial `refresh()`
 * + optimistic `markSurfaceDetached()` on pop-out cover that window.
 */
export function initDetachedSurfaceTracking(): void {
	if (started || isDetachedWindow()) return;
	// Detached windows are a native-shell concept; there is no browser
	// counterpart and no event bus to subscribe to. Without this guard the
	// two `listen()` calls below throw at boot in a remote session, because
	// `@tauri-apps/api/event` reaches straight into `__TAURI_INTERNALS__`.
	if (!isTauri()) {
		console.log('[transport] api/event (detached-surfaces) is desktop-only — deferred to Wave 2');
		return;
	}
	started = true;
	void syncDetachedSurfaces();
	void listen(WINDOW_TOPICS.opened, () => void syncDetachedSurfaces());
	void listen(WINDOW_TOPICS.closed, () => void syncDetachedSurfaces());
	void listen<SurfacesChangedEnvelope>(SURFACES_CHANGED_TOPIC, (ev) => {
		const change = ev.payload?.payload;
		if (change && typeof change.label === 'string' && Array.isArray(change.surface_set)) {
			handleSurfacesChanged({
				label: change.label,
				surface_set: change.surface_set,
				added: change.added ?? [],
				removed: change.removed ?? [],
				move_back: change.move_back === true,
			});
		}
	});
	// Window 2 ⋯ → Make dispatch target (D-09), sent `emitTo('main', …)`.
	void listen<MakeTargetRequest>(MAKE_TARGET_TOPIC, (ev) => handleMakeTargetRequest(ev.payload));
}

/**
 * Optimistically record a surface as detached the instant pop-out is issued,
 * before the `window://opened` round-trip lands — so the primary pane swaps to
 * its placeholder with no duplicate-render flash. Reconciled by the next
 * `refresh()`.
 */
export function markSurfaceDetached(surfaceId: string, label: string): void {
	useDetachedSurfaces.setState((prev) => ({
		surfaceToWindow: { ...prev.surfaceToWindow, [surfaceId]: label },
	}));
}

/** Drop a provisional (still-resolving) Pop out entry — the failure path of
 *  `popOutSurface`, before it re-syncs. A real entry is left alone. */
export function clearPendingSurface(surfaceId: string): void {
	if (useDetachedSurfaces.getState().surfaceToWindow[surfaceId] !== PENDING_WINDOW_LABEL) return;
	useDetachedSurfaces.setState((prev) => {
		const next = { ...prev.surfaceToWindow };
		delete next[surfaceId];
		return { surfaceToWindow: next };
	});
}

/** The surfaces a detached window holds, in map order (WP-69: one or more). */
export function windowSurfaces(label: string): string[] {
	return surfacesOf(useDetachedSurfaces.getState().surfaceToWindow, label);
}

/**
 * Reclaim a popped-out surface back into the primary window: take it out of
 * its detached window (WP-69: that window keeps its other tabs and closes only
 * when this was its last surface; the underlying PTY / file is unaffected) and
 * drop it from the map so the pane re-mounts the live surface inline.
 * Optimistic, with a reconciling `refresh()` on failure.
 */
export async function reclaimSurface(surfaceId: string): Promise<void> {
	const label = useDetachedSurfaces.getState().surfaceToWindow[surfaceId];
	// Nothing to reclaim yet while its Pop out is still resolving.
	if (!label || label === PENDING_WINDOW_LABEL) return;
	// T-3a: arm the reclaim nudge optimistically, same spirit as the
	// optimistic map delete below — the pane view should nudge as soon as it
	// remounts the live surface, not wait on the `window://closed` round trip.
	// Terminals only — see the matching note in the map-diff above. Nothing
	// clears an entry armed for a non-terminal surface.
	if (surfaceId.startsWith('terminal:')) pendingReclaimNudge.add(surfaceId);
	useDetachedSurfaces.setState((prev) => {
		const next = { ...prev.surfaceToWindow };
		delete next[surfaceId];
		return { surfaceToWindow: next };
	});
	try {
		await windowRemoveSurface(label, surfaceId);
	} catch (e) {
		console.warn('detached-surfaces: reclaim failed', e);
		// The close didn't actually happen — undo the optimistic nudge arm too,
		// or a future genuine reclaim isn't what would consume it.
		pendingReclaimNudge.delete(surfaceId);
		void syncDetachedSurfaces();
	}
}

/** Reactive selector — true when `surfaceId` is currently open in a detached window. */
export function useIsSurfaceDetached(surfaceId: string | null | undefined): boolean {
	return useDetachedSurfaces((s) => (surfaceId ? surfaceId in s.surfaceToWindow : false));
}
