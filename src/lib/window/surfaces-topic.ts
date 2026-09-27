// WP-69 (G-SEATS §4.4, DEC-69d) — the `window://surfaces-changed` wire and
// the pure map arithmetic both windows share.
//
// A detached window's `surface_set` used to be fixed at spawn. With Pop out
// joining "Window 2" it grows (a join) and shrinks (Move back to main window,
// or the primary reclaiming one tab). Rust emits each change to the changed
// window and to `main` (`window/registry.rs::emit_surfaces_changed`). The
// topic is host-only for now: `@ikenga/contract`'s `WINDOW_TOPICS` doesn't
// mirror it yet, so it is named here (and in `window/events.rs::topics`).

/** Mirrors Rust `topics::SURFACES_CHANGED`. */
export const SURFACES_CHANGED_TOPIC = 'window://surfaces-changed';

/**
 * Window 2 ⋯ → *Make dispatch target* (D-09 `d9Win2` "Pane actions"): the
 * thin window can't write the primary's shell store, so it asks `main` over
 * this FE-only topic (`emitTo('main', …)`, payload {@link MakeTargetRequest})
 * and the primary sets `companion.activeTarget`. No Rust side.
 */
export const MAKE_TARGET_TOPIC = 'window://make-target';

export interface MakeTargetRequest {
	/** The surface whose session should become the dispatch target. */
	surfaceId: string;
}

/** Mirrors Rust `registry::SurfacesChanged`. */
export interface SurfacesChangedPayload {
	label: string;
	/** The window's full `surface_set` after the change. */
	surface_set: string[];
	added: string[];
	removed: string[];
	/** A *Move back to main window*: the primary mounts the surface again. */
	move_back: boolean;
}

/** The `WindowEventEnvelope` the payload rides (`v`, `topic`, `target`, …). */
export interface SurfacesChangedEnvelope {
	v: number;
	topic: string;
	source_label: string;
	target: { kind: 'broadcast' } | { kind: 'window'; label: string };
	payload: SurfacesChangedPayload;
}

/**
 * The provisional label a surface carries between the Pop out click and the
 * join/spawn resolving, so the origin pane swaps to its placeholder at once.
 * Never a real window label (real ones start `detached-`).
 */
export const PENDING_WINDOW_LABEL = '__window-2-pending__';

/**
 * Apply one change to a `surfaceId → window label` map: the changed window
 * now holds exactly `surface_set`. Entries pointing at other windows are
 * untouched (a move between windows arrives as two changes). Pure.
 */
export function applySurfacesChanged(
	map: Record<string, string>,
	change: Pick<SurfacesChangedPayload, 'label' | 'surface_set'>
): Record<string, string> {
	const next: Record<string, string> = {};
	for (const [surfaceId, label] of Object.entries(map)) {
		if (label !== change.label) next[surfaceId] = label;
	}
	for (const surfaceId of change.surface_set) next[surfaceId] = change.label;
	return next;
}

/** Surface ids a window holds, in map order. Pure. */
export function surfacesOf(map: Record<string, string>, label: string): string[] {
	return Object.entries(map)
		.filter(([, l]) => l === label)
		.map(([surfaceId]) => surfaceId);
}

/**
 * Surfaces that were in a detached window in `prev`, are in none in `next`,
 * and whose window is gone (not in `liveLabels`) — i.e. returned to the main
 * window because their window closed. Grouped by the closed window's label.
 * Provisional entries never count. Pure.
 */
export function returnedByClosedWindows(
	prev: Record<string, string>,
	next: Record<string, string>,
	liveLabels: ReadonlySet<string>
): Record<string, string[]> {
	const out: Record<string, string[]> = {};
	for (const [surfaceId, label] of Object.entries(prev)) {
		if (label === PENDING_WINDOW_LABEL) continue;
		if (surfaceId in next) continue;
		if (liveLabels.has(label)) continue;
		if (!out[label]) out[label] = [];
		out[label].push(surfaceId);
	}
	return out;
}
