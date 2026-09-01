// Iyke control listener — Rust → FE direction.
//
// The Rust HTTP server's write-side handlers (POST /iyke/go, /mode,
// /open, /split, /focus, /close) emit Tauri events instead of mutating
// React state directly. This listener subscribes to those events and
// dispatches them into the right Zustand store. Mounted exactly once,
// at the workspace level (matches `useIykeShellSync` for the read side).
//
// Validation philosophy: the Rust handler did basic sanity checks
// (path starts with /, mode is in the activity-mode union, kind is one
// of the known view kinds). This listener does the *type-aware* check
// against the FE union — anything it can't classify is logged and
// dropped, never thrown, so a bad command from a misbehaving CLI/MCP
// can't crash the UI.

import { listen, type UnlistenFn } from '@/lib/transport';
import { useEffect } from 'react';

import { usePaneStore } from '@/lib/panes/pane-store';
import type { PaneView } from '@/lib/panes/types';
import { modeForRoute } from '@/lib/shell/mode-routes';
import {
	ACTIVITY_MODES,
	type ActivityMode,
	isPkgMode,
	useShellStore,
} from '@/lib/shell/shell-store';
import { createTerminalSession } from '@/terminal/single-terminal';

interface GoPayload {
	path: string;
}
interface ModePayload {
	mode: string;
}
interface SidebarPayload {
	action: 'toggle' | 'open' | 'close';
}
interface SplitPayload {
	direction: 'horizontal' | 'vertical';
	pane_id?: string | null;
}
interface FocusPayload {
	pane_id?: string | null;
	index?: number | null;
}
interface ClosePayload {
	pane_id?: string | null;
}
interface RefreshPayload {
	pane_id?: string | null;
}

/**
 * Mounts listeners for `iyke:*` Tauri events. Returns nothing — the
 * effect cleans up on unmount.
 */
export function useIykeControlListener(): void {
	useEffect(() => {
		const unlisteners: UnlistenFn[] = [];
		let cancelled = false;

		function track(p: Promise<UnlistenFn>) {
			p.then((u) => {
				if (cancelled) {
					u();
				} else {
					unlisteners.push(u);
				}
			}).catch((err) => {
				console.error('[iyke] control listener failed to subscribe:', err);
			});
		}

		track(
			listen<GoPayload>('iyke:go', (e) => {
				const path = e.payload?.path;
				if (typeof path !== 'string' || !path.startsWith('/')) {
					console.warn('[iyke] iyke:go ignored — bad path:', path);
					return;
				}
				usePaneStore.getState().navigateFocused(path);
				// Keep the activity-bar mode coherent with where we just
				// navigated. Only routes that *exclusively* belong to a system
				// mode (Packages / Ngwa / Settings) flip it; shared routes
				// (/sessions, /artifacts, …) return null and leave it untouched.
				const mode = modeForRoute(path);
				if (mode) useShellStore.getState().setActiveMode(mode);
			})
		);

		track(
			listen<ModePayload>('iyke:mode', (e) => {
				const mode = e.payload?.mode;
				// CORE modes are allow-listed; dynamic `pkg:<id>` modes (one per
				// installed app pkg) pass by prefix. A stale pkg mode reconciles
				// to 'app' in the activity bar, so we needn't check the live set.
				const valid =
					typeof mode === 'string' &&
					(ACTIVITY_MODES.includes(mode as ActivityMode) || isPkgMode(mode));
				if (!valid) {
					console.warn('[iyke] iyke:mode ignored — unknown mode:', mode);
					return;
				}
				useShellStore.getState().setActiveMode(mode as ActivityMode);
			})
		);

		track(
			listen<SidebarPayload>('iyke:sidebar', (e) => {
				const action = e.payload?.action ?? 'toggle';
				const store = useShellStore.getState();
				if (action === 'toggle') {
					store.toggleSidebar();
				} else if (action === 'open') {
					store.setSidebarCollapsed(false);
				} else if (action === 'close') {
					store.setSidebarCollapsed(true);
				} else {
					console.warn('[iyke] iyke:sidebar ignored — unknown action:', action);
				}
			})
		);

		track(
			listen<unknown>('iyke:open', (e) => {
				const view = paneViewFromOpenPayload(e.payload);
				if (!view) return;
				const focusedId = usePaneStore.getState().focusedId;
				usePaneStore.getState().addTab(focusedId, view);
			})
		);

		track(
			listen<SplitPayload>('iyke:split', (e) => {
				const direction = e.payload?.direction;
				if (direction !== 'horizontal' && direction !== 'vertical') {
					console.warn('[iyke] iyke:split ignored — bad direction:', direction);
					return;
				}
				const paneId = e.payload?.pane_id;
				if (paneId) {
					usePaneStore.getState().splitPane(paneId, direction);
				} else {
					usePaneStore.getState().splitFocused(direction);
				}
			})
		);

		track(
			listen<FocusPayload>('iyke:focus', (e) => {
				const { pane_id: paneId, index } = e.payload ?? {};
				if (typeof paneId === 'string' && paneId.length > 0) {
					usePaneStore.getState().focusPane(paneId);
					return;
				}
				if (typeof index === 'number' && index >= 1) {
					// CLI/MCP use 1-based indexing to match ⌃1..⌃6 keyboard shortcuts.
					usePaneStore.getState().focusByIndex(index - 1);
					return;
				}
				console.warn('[iyke] iyke:focus ignored — neither pane_id nor index given');
			})
		);

		track(
			listen<ClosePayload>('iyke:close', (e) => {
				const paneId = e.payload?.pane_id;
				if (paneId) {
					usePaneStore.getState().closePane(paneId);
				} else {
					usePaneStore.getState().closeFocusedPane();
				}
			})
		);

		track(
			listen<RefreshPayload>('iyke:refresh', (e) => {
				const paneId = e.payload?.pane_id ?? undefined;
				usePaneStore.getState().refreshPane(paneId);
			})
		);

		return () => {
			cancelled = true;
			for (const u of unlisteners) u();
		};
	}, []);
}

/**
 * Translate the loosely-typed `/iyke/open` payload into a `PaneView`.
 * Returns `null` for anything that doesn't match a known view kind —
 * the caller should drop the event and log a warning.
 */
function paneViewFromOpenPayload(payload: unknown): PaneView | null {
	if (!payload || typeof payload !== 'object') {
		console.warn('[iyke] iyke:open ignored — non-object payload:', payload);
		return null;
	}
	const p = payload as Record<string, unknown>;
	const kind = p.kind;

	if (kind === 'route') {
		const path = p.path;
		if (typeof path !== 'string' || !path.startsWith('/')) {
			console.warn('[iyke] iyke:open route ignored — bad path:', path);
			return null;
		}
		return { kind: 'route', path };
	}

	if (kind === 'terminal') {
		// `cmd` is a free-form string from the CLI ("npm run dev") —
		// shell-words would be ideal but adding a dep for one call is
		// overkill. Plain whitespace split is the same thing the existing
		// ⌘T handler does (it doesn't accept args at all), so this is
		// strictly more capable than the keyboard path.
		const cmdRaw = p.cmd;
		const cmd =
			typeof cmdRaw === 'string' && cmdRaw.trim().length > 0 ? cmdRaw.split(/\s+/) : undefined;
		const sessionId = createTerminalSession(cmd ? { cmd, title: cmdRaw as string } : undefined);
		return { kind: 'terminal', sessionId };
	}

	if (kind === 'artifact') {
		const path = p.path;
		if (typeof path !== 'string' || path.length === 0) {
			console.warn('[iyke] iyke:open artifact ignored — missing path');
			return null;
		}
		return { kind: 'artifact', path };
	}

	// `artifact-grid` is accepted as a wire-protocol alias for the
	// unified artifact-studio at grid density (the iyke skill JSON
	// still emits the old kind name until Phase 6 ships the new verbs).
	if (kind === 'artifact-grid' || kind === 'artifact-studio') {
		const path = p.path;
		if (typeof path !== 'string' || path.length === 0) {
			console.warn(`[iyke] iyke:open ${kind} ignored — missing path`);
			return null;
		}
		const rawDensity = p.density;
		const density: 'grid' | 'loupe' | 'compare' =
			rawDensity === 'loupe' || rawDensity === 'compare'
				? rawDensity
				: kind === 'artifact-grid'
					? 'grid'
					: 'loupe';
		const vs = typeof p.vs === 'string' ? p.vs : undefined;
		return { kind: 'artifact-studio', path, density, vs };
	}

	console.warn('[iyke] iyke:open ignored — unknown kind:', kind);
	return null;
}
