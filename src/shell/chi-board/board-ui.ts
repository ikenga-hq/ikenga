// WP-68 — the seat board's own UI state (D-09 `seats-board.html`, DEC-66/67).
//
// A leaf module (zustand + a type import only) so the rail's
// `openSeatBoard()` in `companion/seat-actions.ts` can ask for board focus
// without an import cycle through `board-store.ts`, which imports the rail's
// actions. `board-store.ts` re-exports everything here.
//
// **Selection is board-local.** Browsing board rows never retargets dispatch
// (D-09 board rule): the rail's selection IS the dispatch target (§9.1), so
// the board keeps its own and only *Make dispatch target* moves the target,
// through the rail's own `makeTarget`. Not persisted — like the rail's.

import { create } from 'zustand';
import type { BoardSelection } from './board-model';

interface BoardUiState {
	/** The board's selected row; `null` → no detail column. */
	selection: BoardSelection | null;
	/** Bumped by an entry point, so a mounted board re-reads `focusPending`. */
	focusRequest: number;
	/** An entry point asked for keyboard focus on the selected row; the
	 *  board consumes it once (a later remount never steals focus). */
	focusPending: boolean;
}

export const useBoardUi = create<BoardUiState>(() => ({ selection: null, focusRequest: 0, focusPending: false }));

export function selectBoardRow(sel: BoardSelection | null): void {
	useBoardUi.setState({ selection: sel });
}

/** Ask the (possibly not yet mounted) board to focus its selected row. Every
 *  entry point does, through `openSeatBoard()`. */
export function requestBoardFocus(): void {
	useBoardUi.setState((s) => ({ focusRequest: s.focusRequest + 1, focusPending: true }));
}

/** The board takes a pending focus request (true once per request). */
export function consumeBoardFocus(): boolean {
	if (!useBoardUi.getState().focusPending) return false;
	useBoardUi.setState({ focusPending: false });
	return true;
}

/** Test seam. */
export function __resetBoardUiForTests(): void {
	useBoardUi.setState({ selection: null, focusRequest: 0, focusPending: false });
}
