/**
 * Paints the engine's `OverlayView` in a DOM layer above xterm's screen.
 *
 * Nothing here writes to the terminal: each overlay cell is an absolutely
 * positioned box with an opaque background covering the server's cell below
 * it. Removing the layer's children is a complete rollback. Works the same
 * over the WebGL and DOM renderers, since both draw inside `.xterm-screen`.
 */

import type { Terminal } from '@xterm/xterm';
import type { OverlayView } from './engine';

const FALLBACK_BG = '#0a0a0a';
const FALLBACK_FG = '#e6e6e6';

export class EchoOverlay {
	private layer: HTMLDivElement | null = null;
	private last: OverlayView | null = null;

	constructor(private readonly term: Terminal) {}

	private ensureLayer(): HTMLDivElement | null {
		const screen = this.term.element?.querySelector<HTMLElement>('.xterm-screen');
		if (!screen) return null;
		if (this.layer && this.layer.parentElement === screen) return this.layer;
		const layer = document.createElement('div');
		layer.className = 'ikenga-local-echo';
		layer.setAttribute('data-testid', 'local-echo-overlay');
		layer.setAttribute('aria-hidden', 'true');
		Object.assign(layer.style, {
			position: 'absolute',
			inset: '0',
			pointerEvents: 'none',
			zIndex: '10',
			overflow: 'hidden',
		});
		screen.appendChild(layer);
		this.layer = layer;
		return layer;
	}

	/** Paint `view` (null clears). Call again after scroll / render / theme. */
	render(view: OverlayView | null): void {
		this.last = view;
		const layer = view ? this.ensureLayer() : this.layer;
		if (!layer) return;
		layer.replaceChildren();
		if (!view) return;
		const b = this.term.buffer.active;
		const vr = view.row - b.viewportY;
		if (vr < 0 || vr >= this.term.rows) return;

		const screen = layer.parentElement as HTMLElement;
		const cellW = screen.clientWidth / this.term.cols || 8;
		const cellH = screen.clientHeight / this.term.rows || 16;
		const opts = this.term.options;
		const theme = opts.theme ?? {};
		const bg = theme.background ?? FALLBACK_BG;
		const fg = theme.foreground ?? FALLBACK_FG;
		const cursorBg = theme.cursor ?? fg;
		const cursorFg = theme.cursorAccent ?? bg;

		for (const c of view.cells) {
			const el = document.createElement('span');
			el.textContent = c.ch === '\u0000' ? '' : c.ch;
			el.setAttribute('data-x', String(c.x));
			if (c.predicted) el.setAttribute('data-predicted', '');
			if (c.cursor) el.setAttribute('data-cursor', '');
			Object.assign(el.style, {
				position: 'absolute',
				left: `${c.x * cellW}px`,
				top: `${vr * cellH}px`,
				width: `${cellW}px`,
				height: `${cellH}px`,
				lineHeight: `${cellH}px`,
				fontFamily: opts.fontFamily ?? 'monospace',
				fontSize: `${opts.fontSize ?? 13}px`,
				textAlign: 'center',
				whiteSpace: 'pre',
				overflow: 'hidden',
				background: c.cursor ? cursorBg : bg,
				color: c.cursor ? cursorFg : c.predicted ? `color-mix(in srgb, ${fg} 62%, ${bg})` : fg,
				textDecoration: c.predicted ? 'underline' : 'none',
				textUnderlineOffset: '2px',
			});
			layer.appendChild(el);
		}
	}

	/** Re-paint the last view at the current geometry / scroll position. */
	refresh(): void {
		this.render(this.last);
	}

	dispose(): void {
		this.layer?.remove();
		this.layer = null;
		this.last = null;
	}
}
