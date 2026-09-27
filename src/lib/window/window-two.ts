// WP-69 — *Pop out* joins "Window 2" (G-SEATS §4.4, DEC-69d, pin P-7).
//
// Window 2 is the most recently focused live non-`main` window that isn't a
// `Workspace` window bound to another project. Rust picks it and adds the
// surface in one call (`window_join_surface`), so the chosen window can't
// close between the pick and the add. When there is no Window 2 the pop-out
// spawns one exactly as before (`window_spawn`, a thin `single-surface`
// window). Joining surfaces the window: a `WebviewWindow` lookup by label,
// then `unminimize()` + `setFocus()` (research §Phase 7, `02`).
//
// A pop-out is a window operation only: nothing here touches a seat (§4.4,
// D-09 rule 2). Callers say what moved (the seat toast lives with the seat
// menu).

import { spawnWindow, windowJoinSurface, windowRemoveSurface } from '@/lib/tauri-cmd';
import { isTauri } from '@/lib/transport';
import { clearPendingSurface, markSurfaceDetached, syncDetachedSurfaces } from './detached-surfaces';
import { PENDING_WINDOW_LABEL } from './surfaces-topic';

export interface PopOutResult {
	/** The window now holding the surface. */
	label: string;
	/** True when it joined an existing Window 2; false when one was spawned. */
	joined: boolean;
}

/** A fresh detached-window label. Must start `detached-`: that is the
 *  capability glob (`capabilities/window-detached.json`). */
export function newDetachedLabel(kind = 'terminal'): string {
	return `detached-${kind}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

/**
 * Pop `surfaceId` out to Window 2: join the existing one, else spawn it.
 *
 * Optimistic: the surface is marked detached (provisionally) before the IPC,
 * so the origin pane swaps to its placeholder at once instead of briefly
 * duplicating the live surface. A failure clears that mark and re-syncs from
 * the registry, then rethrows.
 */
export async function popOutSurface(
	surfaceId: string,
	opts: { projectId: string | null; kind?: string }
): Promise<PopOutResult> {
	markSurfaceDetached(surfaceId, PENDING_WINDOW_LABEL);
	try {
		const joined = await windowJoinSurface(surfaceId, opts.projectId);
		if (joined) {
			markSurfaceDetached(surfaceId, joined);
			void focusWindow(joined);
			return { label: joined, joined: true };
		}
		const label = newDetachedLabel(opts.kind);
		await spawnWindow({
			label,
			kind: 'single-surface',
			surface_set: [surfaceId],
			project_id: null,
			layout_key: label,
		});
		markSurfaceDetached(surfaceId, label);
		return { label, joined: false };
	} catch (err) {
		clearPendingSurface(surfaceId);
		void syncDetachedSurfaces();
		throw err;
	}
}

/**
 * Bring a window to the front: `WebviewWindow` lookup by label, then
 * restore it if minimised and focus it. Best-effort — resolves `false` when
 * the window isn't found or the platform refuses (the join itself stands).
 * Needs `core:window:allow-set-focus` / `allow-unminimize` on the caller's
 * capability (`capabilities/default.json` for `main`).
 */
export async function focusWindow(label: string): Promise<boolean> {
	if (!isTauri()) return false;
	try {
		const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
		const w = await WebviewWindow.getByLabel(label);
		if (!w) return false;
		await w.unminimize().catch(() => {});
		await w.setFocus();
		return true;
	} catch (e) {
		console.warn(`[window-two] focus '${label}' failed`, e);
		return false;
	}
}

/**
 * Window 2 ⋯ → *Move back to main window* (D-09): take `surfaceId` out of
 * this window. The primary mounts it again (`onSurfacesReturned`), and the
 * window closes when that was its last tab.
 */
export function moveSurfaceBack(label: string, surfaceId: string): Promise<string[]> {
	return windowRemoveSurface(label, surfaceId, true);
}
