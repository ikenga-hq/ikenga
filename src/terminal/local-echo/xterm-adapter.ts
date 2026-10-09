/**
 * `EchoTerm` over a real xterm.js `Terminal` — the only place the engine's
 * view of the screen touches xterm's API.
 */

import type { IBufferCell, Terminal } from '@xterm/xterm';
import type { CellView, EchoTerm, RowHandle } from './engine';

/** Palette index 8 ("bright black"): what zsh-autosuggestions and most
 *  ghost-text hints are drawn in when they are not drawn faint. */
const GHOST_PALETTE_INDEX = 8;

/**
 * Is this cell a hint the app will replace on the next edit (an
 * autosuggestion, a placeholder) rather than text? Faint, bright-black, a
 * mid grey from the 256-colour ramp, or a mid grey in truecolour (fish's
 * default autosuggestion is `555`). A real character misread as a hint only
 * costs a flicker: the prediction blanks it until the server's row returns.
 */
function isGhost(cell: IBufferCell): boolean {
	if (cell.isDim() !== 0) return true;
	if (cell.isFgPalette()) {
		const c = cell.getFgColor();
		return c === GHOST_PALETTE_INDEX || (c >= 238 && c <= 247);
	}
	if (cell.isFgRGB()) {
		const v = cell.getFgColor();
		const r = (v >> 16) & 0xff;
		const g = (v >> 8) & 0xff;
		const b = v & 0xff;
		return Math.max(r, g, b) - Math.min(r, g, b) <= 16 && r >= 0x40 && r <= 0x99;
	}
	return false;
}

export function createXtermEchoTerm(term: Terminal, isCursorHidden: () => boolean): EchoTerm {
	let scratch: IBufferCell | undefined;
	return {
		get cols() {
			return term.cols;
		},
		isAltScreen: () => term.buffer.active.type === 'alternate',
		isCursorHidden,
		cursor: () => {
			const b = term.buffer.active;
			return { x: b.cursorX, y: b.baseY + b.cursorY };
		},
		readRow: (y) => {
			const line = term.buffer.active.getLine(y);
			if (!line) return null;
			const cells: CellView[] = [];
			for (let x = 0; x < term.cols; x++) {
				const cell = line.getCell(x, scratch);
				if (!cell) {
					cells.push({ ch: ' ', ghost: false });
					continue;
				}
				scratch = cell;
				const ghost = isGhost(cell);
				// A wide glyph's trailing cell (width 0) has no chars of its
				// own; give it a marker that can never equal a predicted
				// character, so a row holding wide glyphs is never "confirmed"
				// by accident.
				const ch = cell.getWidth() === 0 ? '\u0000' : cell.getChars() || ' ';
				cells.push({ ch, ghost });
			}
			return cells;
		},
		trackRow: (y) => {
			const b = term.buffer.active;
			const marker = term.registerMarker(y - (b.baseY + b.cursorY));
			if (!marker) {
				// No marker (should not happen on xterm 5.5): follow the
				// absolute row and let validation catch any drift.
				return { line: y, dispose: () => {} } satisfies RowHandle;
			}
			return {
				get line() {
					return marker.isDisposed ? -1 : marker.line;
				},
				dispose: () => marker.dispose(),
			};
		},
	};
}
