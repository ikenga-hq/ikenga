// Transient drag state for tab DnD. Lives outside pane-store so that drag
// start/end mutations don't churn the persistence subscriber. Set and cleared
// by the pointer-drag controller (`pointer-drag.ts`) via tab sources.
//
// `source` discriminates pane-tab drags from dock-tab drags so drop targets
// can pull the source view from the right store. Pane-source drags carry
// `srcLeafId`; dock-source drags leave it null and use `srcTabIdx` against
// `useDockStore.getState().tabs`.
//
// `external` drags (WP-67: a Chi seat row dragged onto a pane, the gesture
// ported from D-09's Explorer variant) carry no tab index at all: the source
// hands a `dropAt` callback that places whatever it represents, and the drop
// zone only reports where the drop landed.

import { create } from 'zustand';
import type { MoveTabMode } from './pane-reducer';

export type DragSource = 'pane' | 'dock' | 'external';

/** Place an external drag's payload at a pane (`mode`: centre = tab, edge =
 *  split). Returns whether it was placed. */
export type ExternalDropAt = (paneId: string, mode: MoveTabMode) => boolean;

interface DragState {
	active: boolean;
	source: DragSource | null;
	srcLeafId: string | null;
	srcTabIdx: number | null;
	/** Set only for `external` drags. */
	dropAt: ExternalDropAt | null;
	startPane: (leafId: string, tabIdx: number) => void;
	startDock: (tabIdx: number) => void;
	startExternal: (dropAt: ExternalDropAt) => void;
	end: () => void;
}

export const useDragState = create<DragState>((set) => ({
	active: false,
	source: null,
	srcLeafId: null,
	srcTabIdx: null,
	dropAt: null,
	startPane: (leafId, tabIdx) =>
		set({ active: true, source: 'pane', srcLeafId: leafId, srcTabIdx: tabIdx, dropAt: null }),
	startDock: (tabIdx) =>
		set({ active: true, source: 'dock', srcLeafId: null, srcTabIdx: tabIdx, dropAt: null }),
	startExternal: (dropAt) =>
		set({ active: true, source: 'external', srcLeafId: null, srcTabIdx: null, dropAt }),
	end: () => set({ active: false, source: null, srcLeafId: null, srcTabIdx: null, dropAt: null }),
}));
