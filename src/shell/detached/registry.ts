// Detached surface-host registry (plans/multi-window WP-05 + WP-06).
//
// A detached window mounts the surfaces named in its descriptor's
// `surface_set` (Flavor C = one; WP-69: a Pop out that joins Window 2 adds
// more, shown as tabs — see `detached-root.tsx`). This registry is the single seam that maps
// a `surface_id` → { React component, the live-sync event topic }. The
// component is `React.lazy`-loaded so the thin entry only parses the bundle
// for the surface it actually mounts — the performance lever.
//
// WP-06 registers the `viewer` surface here.
//
// Surface-set id convention (WP-06):
//   `"viewer:<path>"` → resolves to the `viewer` surface; the component
//   extracts the file path suffix. First-colon split only so absolute paths
//   (e.g. `/home/user/file.md`) survive intact.
//   Plain `"placeholder"` (no colon) → resolve by exact key.
//
// `resolveSurface` checks for an exact key first, then falls back to a
// prefix match (the substring before the first `:`). This keeps the registry
// keys clean ("terminal", "viewer") while allowing context to travel in the id.

import { type ComponentType, type LazyExoticComponent, lazy, type ReactNode } from 'react';

import type { WindowContext } from '@/lib/window/window-context';
import type { WindowLifecycleState } from './use-window-lifecycle';

/** Props every detached surface body receives from the thin root. */
export interface DetachedSurfaceProps {
	/** This window's identity (label, kind, surface_set, project binding). */
	ctx: WindowContext;
	/** Live `window://` lifecycle state, subscribed once at the root. */
	lifecycle: WindowLifecycleState;
	/**
	 * WP-69: the window's ⋯ (Move back to main window · Close Window 2), for a
	 * surface that `ownsActions` to place in its own header — D-09 draws it at
	 * the end of the pane's address row. `ctx.surfaces` is always this
	 * surface's own id alone, even when the window holds several as tabs.
	 */
	actions?: ReactNode;
}

export interface DetachedSurface {
	/** Stable base id — matches the prefix of an entry in `surface_set`. */
	id: string;
	/** Human label for the window chrome / title. */
	title: string;
	/** Lazy body. Default-exports a `ComponentType<DetachedSurfaceProps>`. */
	component: LazyExoticComponent<ComponentType<DetachedSurfaceProps>>;
	/**
	 * Optional Tauri event topic the surface stays in sync over. Documented
	 * here so the substrate, not each surface, owns the wiring. The viewer
	 * surface reads files directly and needs no cross-window topic.
	 */
	topic?: string;
	/** WP-69: renders `actions` in its own header; otherwise the root gives
	 *  the ⋯ a slim row above the surface. */
	ownsActions?: boolean;
}

const SURFACES: Record<string, DetachedSurface> = {
	placeholder: {
		id: 'placeholder',
		title: 'Detached surface',
		component: lazy(() => import('./surfaces/placeholder-surface')),
	},
	viewer: {
		id: 'viewer',
		title: 'Viewer',
		component: lazy(() => import('./surfaces/viewer-surface')),
	},
	// WP-08: terminal pop-out. Attaches to the shared core PTY over its
	// broadcast `pty://<id>` stream (the live-sync topic); both windows drive
	// the same shell.
	terminal: {
		id: 'terminal',
		title: 'Terminal',
		topic: 'pty://*',
		ownsActions: true,
		component: lazy(() => import('./surfaces/terminal-surface')),
	},
};

/**
 * Resolve a surface by id. Checks for an exact key match first, then falls
 * back to a prefix match (the portion of `id` before the first `:`). This
 * lets `"viewer:<path>"` resolve to its base surface while the context
 * suffix travels through to the component via `ctx.surfaces[0]`.
 */
export function resolveSurface(id: string): DetachedSurface | undefined {
	if (SURFACES[id]) return SURFACES[id];
	const colon = id.indexOf(':');
	if (colon > 0) {
		return SURFACES[id.slice(0, colon)];
	}
	return undefined;
}

/** Every registered base surface id (for diagnostics / the unknown-surface hint). */
export function registeredSurfaceIds(): string[] {
	return Object.keys(SURFACES);
}
