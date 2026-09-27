// WP-68 — the seat board's own UI state and its entry point (D-09
// `seats-board.html`, DEC-66/67, G-SEATS §8).
//
// **Selection is board-local.** Browsing board rows never retargets dispatch
// (D-09 board rule): the rail's selection IS the dispatch target (§9.1), so
// the board keeps its own and only *Make dispatch target* moves the target,
// through the rail's own `makeTarget`. Not persisted — like the rail's.
//
// **Every entry point** (⌘2 again from dispatch = `chi.board`, the palette's
// "Chi: Open seat board", the Explorer Sessions header's "Seats" link) opens
// `/chi` in the focused pane — reusing the tab when it is already open,
// which `openSeatBoard()` (the rail's ⊞ action) does through the pane
// store's `addTab` — and asks the board to land keyboard focus on its
// selected row.

import { create } from 'zustand';
import { usePaneStore } from '@/lib/panes/pane-store';
import { findLeaf, getLeafIdsInOrder } from '@/lib/panes/pane-reducer';
import type { PaneNode, PaneView } from '@/lib/panes/types';
import { openSeatBoard } from '@/shell/companion/seat-actions';
import type { BoardSelection } from './board-model';

/** The board's route (`src/routes/chi/index.tsx`). */
export const SEAT_BOARD_PATH = '/chi';

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

/** Ask the (possibly not yet mounted) board to focus its selected row. */
export function requestBoardFocus(): void {
	useBoardUi.setState((s) => ({ focusRequest: s.focusRequest + 1, focusPending: true }));
}

/** The board takes a pending focus request (true once per request). */
export function consumeBoardFocus(): boolean {
	if (!useBoardUi.getState().focusPending) return false;
	useBoardUi.setState({ focusPending: false });
	return true;
}

/**
 * Open the seat board in the focused pane (one tab: a second open focuses
 * the tab that is already there) and land keyboard focus on it. The handler
 * of `chi.board`, the palette item and the Explorer link.
 */
export function openBoard(): void {
	openSeatBoard();
	requestBoardFocus();
}

export function isBoardView(view: PaneView | undefined): boolean {
	if (view?.kind !== 'route') return false;
	const path = view.path.split(/[?#]/)[0]?.replace(/\/+$/, '') ?? '';
	return path === SEAT_BOARD_PATH;
}

/** Is the board the active tab of any pane (the Explorer link's `aria-current`)? */
export function boardIsShowing(root: PaneNode): boolean {
	return getLeafIdsInOrder(root).some((id) => {
		const leaf = findLeaf(root, id);
		return isBoardView(leaf?.tabs[leaf.activeTabIdx]);
	});
}

/**
 * D-09: the rail's actions open things "in the focused pane". Asked from the
 * board, that pane is the board itself — so point focus at a pane beside it
 * first and the board stays put. The focused pane wins when it isn't showing
 * the board; otherwise the last pane that isn't. With the board alone in the
 * window nothing moves (the action opens a tab beside the board's, as D-09
 * does).
 */
export function focusBesideBoard(boardPaneId: string | null): void {
	const panes = usePaneStore.getState();
	const showsBoard = (id: string) => {
		const leaf = findLeaf(panes.root, id);
		return id === boardPaneId || isBoardView(leaf?.tabs[leaf.activeTabIdx]);
	};
	if (!showsBoard(panes.focusedId)) return;
	const others = getLeafIdsInOrder(panes.root).filter((id) => !showsBoard(id));
	const pick = others[others.length - 1];
	if (pick) panes.focusPane(pick);
}

/** Test seam. */
export function __resetBoardUiForTests(): void {
	useBoardUi.setState({ selection: null, focusRequest: 0, focusPending: false });
}
