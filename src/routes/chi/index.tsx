// /chi/ — the seat board (WP-68, D-09 `seats-board.html`; DEC-66).
//
// Opening `/chi` puts the board in the focused pane: every seat and every
// unseated session side by side, with a detail column for the selected one.
// The Companion's seat rail stays the place you select, dispatch and act;
// the board draws the same roster and calls the same actions
// (`src/shell/chi-board/`). Entry points: ⌘2 again from the dispatch input
// (`chi.board`), the rail's "All seats" ⊞, the palette's "Chi: Open seat
// board", and the Explorer Sessions header's "Seats" link. The sidebar stays
// the Project Explorer (DEC-66: no Chi sessions list).

import { createFileRoute } from '@tanstack/react-router';
import { SeatBoard } from '@/shell/chi-board/seat-board';

export const Route = createFileRoute('/chi/')({
	component: SeatBoard,
});
