// D-08 pane chrome — per-pane ephemeral viewer UI state (zoom, device-width
// preset, and which chrome variant is showing: the default renderer, the
// source split, or the version-history drawer).
//
// Deliberately NOT part of `PaneView` / the persisted pane tree: this is
// throwaway UI state (closing and reopening the same path resets it), not
// navigation state. It's also NOT local `useState` inside one component,
// because the menu that changes it (`PaneTools`, in the merged address row)
// and the content that reads it (`ArtifactView`, in the pane body) are
// siblings under `Pane`, not parent/child — see pane-toolbar.tsx and
// artifact-view.tsx.
//
// designs/pane-chrome.html — `S.zoom` / `S.device` / the `artifact-history`
// and `artifact-source` states this backs.

import { create } from 'zustand';

export type DeviceWidth = '390' | '768' | 'full';
export type ArtifactVariant = 'default' | 'source' | 'history';

export const ZOOM_MIN = 50;
export const ZOOM_MAX = 200;
export const ZOOM_STEP = 10;

interface PaneViewerState {
	zoom: number;
	device: DeviceWidth;
	variant: ArtifactVariant;
}

const DEFAULT_STATE: PaneViewerState = { zoom: 100, device: 'full', variant: 'default' };

function clampZoom(z: number): number {
	return Math.max(ZOOM_MIN, Math.min(ZOOM_MAX, z));
}

interface ViewerPaneStore {
	panes: Record<string, PaneViewerState>;
	/** Named `forPane`, not `get` — a same-named method here would shadow the
	 *  zustand creator's own `get` parameter in every other method below,
	 *  and read as (but not be) the store's built-in `getState()`. */
	forPane(paneId: string): PaneViewerState;
	zoomBy(paneId: string, delta: number): void;
	resetZoom(paneId: string): void;
	setDevice(paneId: string, device: DeviceWidth): void;
	setVariant(paneId: string, variant: ArtifactVariant): void;
	/** Called by `ArtifactView` on unmount (the pane's active tab changes
	 *  away from the viewer, or the pane closes) — the next artifact opened in
	 *  the pane starts from a clean slate and closed panes leave no entry. */
	reset(paneId: string): void;
}

export const useViewerPaneState = create<ViewerPaneStore>((set, get) => ({
	panes: {},
	forPane(paneId) {
		return get().panes[paneId] ?? DEFAULT_STATE;
	},
	zoomBy(paneId, delta) {
		set((s) => {
			const cur = s.panes[paneId] ?? DEFAULT_STATE;
			return { panes: { ...s.panes, [paneId]: { ...cur, zoom: clampZoom(cur.zoom + delta) } } };
		});
	},
	resetZoom(paneId) {
		set((s) => {
			const cur = s.panes[paneId] ?? DEFAULT_STATE;
			return { panes: { ...s.panes, [paneId]: { ...cur, zoom: 100 } } };
		});
	},
	setDevice(paneId, device) {
		set((s) => {
			const cur = s.panes[paneId] ?? DEFAULT_STATE;
			return { panes: { ...s.panes, [paneId]: { ...cur, device } } };
		});
	},
	setVariant(paneId, variant) {
		set((s) => {
			const cur = s.panes[paneId] ?? DEFAULT_STATE;
			return { panes: { ...s.panes, [paneId]: { ...cur, variant } } };
		});
	},
	reset(paneId) {
		set((s) => {
			const next = { ...s.panes };
			delete next[paneId];
			return { panes: next };
		});
	},
}));
