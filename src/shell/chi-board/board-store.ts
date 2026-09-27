// WP-68 — the seat board's entry point and pane helpers (D-09
// `seats-board.html`, DEC-66/67, G-SEATS §8). The board's UI state
// (selection, focus request) lives in `board-ui.ts` and is re-exported here.
//
// **Every entry point** (⌘2 again from dispatch = `chi.board`, the palette's
// "Chi: Open seat board", the Explorer Sessions header's "Seats" link, the
// rail's ⊞ and its menu's *All seats*) goes through `openSeatBoard()`, which
// opens `/chi` in the focused pane — reusing the tab when it is already open
// (the pane store's `addTab`) — and asks the board to land keyboard focus on
// its selected row (`requestBoardFocus`).

import { usePaneStore } from '@/lib/panes/pane-store';
import { findLeaf, getLeafIdsInOrder } from '@/lib/panes/pane-reducer';
import type { PaneNode, PaneView } from '@/lib/panes/types';
import { openSeatBoard } from '@/shell/companion/seat-actions';

export {
	__resetBoardUiForTests,
	consumeBoardFocus,
	requestBoardFocus,
	selectBoardRow,
	useBoardUi,
} from './board-ui';

/** The board's route (`src/routes/chi/index.tsx`). */
export const SEAT_BOARD_PATH = '/chi';

/**
 * Open the seat board in the focused pane (one tab: a second open focuses
 * the tab that is already there) and land keyboard focus on it. The handler
 * of `chi.board`, the palette item and the Explorer link; the rail's ⊞ calls
 * `openSeatBoard()` directly, which is the same thing.
 */
export function openBoard(): void {
	openSeatBoard();
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
