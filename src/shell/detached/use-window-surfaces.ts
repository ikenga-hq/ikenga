// WP-69 (G-SEATS §4.4, DEC-69d) — the live surface set of THIS detached
// window, and which one is the active tab.
//
// A thin window used to mount exactly the `surface_set` in its spawn URL.
// Pop out now joins an existing window ("Window 2"), and Window 2 ⋯ → Move
// back takes one out, so the set is live: seeded from the URL, reconciled
// once from the registry on mount (a join can land before this listener
// attaches), then kept current by `window://surfaces-changed`, which Rust
// emits to this window's label.

import { useCallback, useEffect, useRef, useState } from 'react';

import { listWindows } from '@/lib/tauri-cmd';
import { isTauri, listen, type UnlistenFn } from '@/lib/transport';
import { SURFACES_CHANGED_TOPIC, type SurfacesChangedEnvelope } from '@/lib/window/surfaces-topic';
import type { WindowContext } from '@/lib/window/window-context';

/**
 * The active tab after the set changes (D-09 `popOut` / `moveBack`): a join
 * activates the surface it added; otherwise the active tab stays when it is
 * still there; when it left, the tab now at its index (clamped) takes over.
 * Pure.
 */
export function nextActiveSurface(
	prevSurfaces: readonly string[],
	prevActive: string | null,
	nextSurfaces: readonly string[],
	added: readonly string[] = []
): string | null {
	if (nextSurfaces.length === 0) return null;
	for (let i = added.length - 1; i >= 0; i--) {
		if (nextSurfaces.includes(added[i])) return added[i];
	}
	if (prevActive && nextSurfaces.includes(prevActive)) return prevActive;
	const at = prevActive ? prevSurfaces.indexOf(prevActive) : 0;
	const idx = Math.max(0, Math.min(at < 0 ? 0 : at, nextSurfaces.length - 1));
	return nextSurfaces[idx] ?? null;
}

export interface WindowSurfaces {
	surfaces: string[];
	active: string | null;
	setActive: (surfaceId: string) => void;
}

export function useWindowSurfaces(ctx: WindowContext): WindowSurfaces {
	const [state, setState] = useState<{ surfaces: string[]; active: string | null }>(() => ({
		surfaces: ctx.surfaces,
		active: ctx.surfaces[0] ?? null,
	}));
	// The latest state for the async listeners below, without re-subscribing.
	const stateRef = useRef(state);
	stateRef.current = state;

	const apply = useCallback((next: string[], added: string[] = []) => {
		setState((prev) => ({
			surfaces: next,
			active: nextActiveSurface(prev.surfaces, prev.active, next, added),
		}));
	}, []);

	useEffect(() => {
		if (!isTauri()) return;
		let cancelled = false;
		let unlisten: UnlistenFn | null = null;
		void listen<SurfacesChangedEnvelope>(SURFACES_CHANGED_TOPIC, (ev) => {
			const change = ev.payload?.payload;
			if (!change || change.label !== ctx.label || !Array.isArray(change.surface_set)) return;
			apply(change.surface_set, change.added ?? []);
		}).then((fn) => {
			if (cancelled) fn();
			else unlisten = fn;
		});
		// Reconcile once: a join issued right after this window spawned can
		// land before the listener above is attached.
		void listWindows()
			.then((windows) => {
				if (cancelled) return;
				const own = windows.find((w) => w.label === ctx.label);
				if (!own) return;
				const cur = stateRef.current.surfaces;
				const same = own.surface_set.length === cur.length && own.surface_set.every((s, i) => s === cur[i]);
				if (!same) apply(own.surface_set, own.surface_set.filter((s) => !cur.includes(s)));
			})
			.catch(() => {
				/* keep the URL's set */
			});
		return () => {
			cancelled = true;
			unlisten?.();
		};
	}, [ctx.label, apply]);

	const setActive = useCallback((surfaceId: string) => {
		setState((prev) => (prev.surfaces.includes(surfaceId) ? { ...prev, active: surfaceId } : prev));
	}, []);

	return { surfaces: state.surfaces, active: state.active, setActive };
}
