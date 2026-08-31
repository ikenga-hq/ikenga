/**
 * os-file-drop — routes native OS file drops to the drop surface under the
 * cursor (terminals → insert path), with a live
 * drop-zone overlay so the target highlights as you drag, the way Zed and
 * other editors do it.
 *
 * Why this exists and why it's shaped this way:
 *
 * In a Tauri webview the browser's HTML5 `drop`/`dragover` events DO NOT fire
 * for OS file drops — the native window layer intercepts the drag before the
 * page sees it, and on WebKitGTK `dataTransfer` is blanked for security. The
 * only source of a dropped file's real path (and of drag-over position updates)
 * is Tauri's `onDragDropEvent`.
 *
 * Tauri gives NO built-in way to know which DOM element a window-level drop
 * landed on (tauri-apps/tauri#13835). The community-standard approach — used
 * here — is to hit-test the position against the `getBoundingClientRect()` of
 * the specific elements we care about, not `document.elementFromPoint` (which
 * resolves to whatever bare element is on top and mis-routed).
 *
 * Coordinate space: `onDragDropEvent` reports a `PhysicalPosition`;
 * `getBoundingClientRect()` is CSS pixels. The conversion factor is
 * `window.devicePixelRatio`, NOT `getCurrentWindow().scaleFactor()` — on this
 * WebKitGTK build under fractional scaling `scaleFactor()` returns 1 while the
 * real ratio is fractional (e.g. 0.8), so `pos / dpr` is what lands on the
 * right element (verified against the live app). We convert by dpr first and
 * fall back to the raw point, so a platform that already reports logical pixels
 * (dpr === 1, or a future Tauri fix) still works.
 *
 * macOS keeps the native handler disabled (it would break in-page pane DnD;
 * see lib.rs), so this never fires there and each surface's HTML5 path stays
 * in charge.
 */

import { isTauri } from '../transport';

export const OS_FILE_DROP_EVENT = 'ikenga:os-file-drop';

/** Elements that accept an OS file drop advertise themselves with one of these
 *  attributes, plus an optional `data-os-drop-label` for the overlay hint. */
const SURFACE_SELECTOR = '[data-terminal-session], [data-os-drop-target]';

export interface OsFileDropDetail {
	paths: string[];
}

function inRect(x: number, y: number, r: DOMRect): boolean {
	return x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;
}

/** The topmost drop surface whose CSS rect contains a point, if any. */
function surfaceAt(x: number, y: number): Element | null {
	// Reverse DOM order so a surface painted on top wins when rects overlap.
	const surfaces = Array.from(document.querySelectorAll(SURFACE_SELECTOR)).reverse();
	return surfaces.find((el) => inRect(x, y, el.getBoundingClientRect())) ?? null;
}

/** Resolve the surface under a physical drop position, converting to CSS px. */
function resolveSurface(px: number, py: number): Element | null {
	const dpr = window.devicePixelRatio || 1;
	return surfaceAt(px / dpr, py / dpr) ?? surfaceAt(px, py);
}

/** A single fixed-position highlight element, lazily created, reused across
 *  drags. `pointer-events: none` so it never eats the drop. */
function ensureOverlay(): HTMLDivElement {
	const existing = document.getElementById('os-file-drop-overlay');
	if (existing) return existing as HTMLDivElement;
	const el = document.createElement('div');
	el.id = 'os-file-drop-overlay';
	Object.assign(el.style, {
		position: 'fixed',
		zIndex: '2147483647',
		pointerEvents: 'none',
		display: 'none',
		boxSizing: 'border-box',
		border: '2px solid var(--primary, #6ea8fe)',
		borderRadius: '8px',
		background: 'color-mix(in srgb, var(--primary, #6ea8fe) 14%, transparent)',
		transition: 'left 60ms ease, top 60ms ease, width 60ms ease, height 60ms ease',
	} as Partial<CSSStyleDeclaration>);
	const label = document.createElement('div');
	label.className = 'os-file-drop-overlay__label';
	Object.assign(label.style, {
		position: 'absolute',
		left: '50%',
		top: '50%',
		transform: 'translate(-50%, -50%)',
		padding: '4px 12px',
		borderRadius: '999px',
		font: '500 12px/1.4 var(--font-sans, system-ui, sans-serif)',
		color: 'var(--primary-fg, #fff)',
		background: 'var(--primary, #6ea8fe)',
		whiteSpace: 'nowrap',
		boxShadow: '0 2px 8px rgba(0,0,0,0.25)',
	} as Partial<CSSStyleDeclaration>);
	el.appendChild(label);
	document.body.appendChild(el);
	return el;
}

function showOverlay(target: Element): void {
	const overlay = ensureOverlay();
	const r = target.getBoundingClientRect();
	overlay.style.left = `${r.left}px`;
	overlay.style.top = `${r.top}px`;
	overlay.style.width = `${r.width}px`;
	overlay.style.height = `${r.height}px`;
	overlay.style.display = 'block';
	const label = overlay.querySelector('.os-file-drop-overlay__label') as HTMLDivElement | null;
	if (label) {
		label.textContent = target.hasAttribute('data-terminal-session')
			? 'Drop to insert path'
			: (target.getAttribute('data-os-drop-label') ?? 'Drop file');
	}
}

function hideOverlay(): void {
	const overlay = document.getElementById('os-file-drop-overlay');
	if (overlay) overlay.style.display = 'none';
}

export async function initOsFileDrop(): Promise<() => void> {
	if (!isTauri()) {
		console.log('[transport] api/webview (os-file-drop) is desktop-only — deferred to Wave 2');
		return () => {};
	}
	const { getCurrentWebview } = await import('@tauri-apps/api/webview');
	const webview = getCurrentWebview();
	const unlisten = await webview.onDragDropEvent((event) => {
		const p = event.payload;
		switch (p.type) {
			case 'enter':
			case 'over': {
				const hit = resolveSurface(p.position.x, p.position.y);
				if (hit) showOverlay(hit);
				else hideOverlay();
				break;
			}
			case 'leave':
				hideOverlay();
				break;
			case 'drop': {
				hideOverlay();
				if (p.paths.length === 0) return;
				const hit = resolveSurface(p.position.x, p.position.y);
				if (!hit) return;
				hit.dispatchEvent(
					new CustomEvent<OsFileDropDetail>(OS_FILE_DROP_EVENT, {
						detail: { paths: p.paths },
						bubbles: false,
					})
				);
				break;
			}
		}
	});
	return () => {
		unlisten();
		hideOverlay();
	};
}
