// Transient drag state for tab DnD. Lives outside pane-store so that drag
// start/end mutations don't churn the persistence subscriber. Set and cleared
// by the pointer-drag controller (`pointer-drag.ts`) via tab sources.
//
// `source` discriminates pane-tab drags from dock-tab drags so drop targets
// can pull the source view from the right store. Pane-source drags carry
// `srcLeafId`; dock-source drags leave it null and use `srcTabIdx` against
// `useDockStore.getState().tabs`.

import { create } from 'zustand';

export type DragSource = 'pane' | 'dock';

interface DragState {
	active: boolean;
	source: DragSource | null;
	srcLeafId: string | null;
	srcTabIdx: number | null;
	startPane: (leafId: string, tabIdx: number) => void;
	startDock: (tabIdx: number) => void;
	end: () => void;
}

export const useDragState = create<DragState>((set) => ({
	active: false,
	source: null,
	srcLeafId: null,
	srcTabIdx: null,
	startPane: (leafId, tabIdx) =>
		set({ active: true, source: 'pane', srcLeafId: leafId, srcTabIdx: tabIdx }),
	startDock: (tabIdx) => set({ active: true, source: 'dock', srcLeafId: null, srcTabIdx: tabIdx }),
	end: () => set({ active: false, source: null, srcLeafId: null, srcTabIdx: null }),
}));
