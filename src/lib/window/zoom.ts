// App-wide zoom (⌘/⌃ + = / - / 0).
//
// One zoom level for the whole shell — chrome, panes, pkg iframes, terminals.
// Implemented at the *webview* level (`Webview.setZoom`) rather than as a CSS
// transform or a per-surface font-size, for three reasons:
//
//   1. It reaches inside pkg iframes and the xterm canvas, neither of which a
//      parent CSS scale composes correctly (canvas re-rasterizes blurry; the
//      iframes are separate documents).
//   2. xterm's FitAddon already re-fits on its ResizeObserver, and zooming
//      changes the container's CSS-pixel box — so cols/rows recompute and the
//      PTY gets a correct SIGWINCH for free. A CSS transform would NOT change
//      the observed box, leaving the pty's idea of the size wrong.
//   3. It's the same mechanism the OS/browser zoom hotkeys use, so text
//      rendering stays hinted at every step instead of being scaled bitmaps.
//
// Every window (primary + Flavor-C detached pop-outs) boots through
// `src/main.tsx`, which calls `installZoom()`. Each webview must apply the
// level to *itself* — `setZoom` is per-webview, not per-app — so a change is
// persisted to localStorage AND broadcast on a Tauri event that every live
// window listens for. Without the broadcast, a pop-out would keep its
// boot-time zoom until relaunch.

import { isTauri, listen } from '../transport';

const STORAGE_KEY = 'ikenga.zoom';
const ZOOM_EVENT = 'ikenga://zoom-changed';

// Discrete ladder rather than a multiplier, so ⌃- then ⌃+ always lands back
// exactly on 1.0 (repeated float multiply/divide drifts off it and never
// re-hits the crisp unzoomed raster).
const LEVELS: number[] = [0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5];
const DEFAULT_LEVEL = 1.0;

function clampToLadder(value: number): number {
	// Snap to the nearest rung — a persisted value from an older ladder (or a
	// hand-edited localStorage entry) must still resolve to something valid.
	let best = LEVELS[0];
	let bestDelta = Math.abs(value - best);
	for (const level of LEVELS) {
		const delta = Math.abs(value - level);
		if (delta < bestDelta) {
			best = level;
			bestDelta = delta;
		}
	}
	return best;
}

function readStored(): number {
	try {
		const raw = localStorage.getItem(STORAGE_KEY);
		if (!raw) return DEFAULT_LEVEL;
		const parsed = Number.parseFloat(raw);
		return Number.isFinite(parsed) ? clampToLadder(parsed) : DEFAULT_LEVEL;
	} catch {
		return DEFAULT_LEVEL;
	}
}

let current = DEFAULT_LEVEL;

/** The zoom level this webview is currently rendering at. */
export function getZoom(): number {
	return current;
}

// Applies to THIS webview only. Tolerates the Vite-only dev server
// (`bun run dev`), where there's no Tauri webview to zoom.
//
// `setZoom` requires the `core:webview:allow-set-webview-zoom` capability —
// it is NOT part of `core:webview:default` (see tauri build.rs, which lists
// set_webview_zoom as default-off). Missing it rejects at the ACL layer. An
// earlier version caught that into a console.warn, which made a hard,
// fixable permission error look like "zoom silently does nothing" — so a
// denial is now surfaced loudly and distinguished from the benign
// no-Tauri-here case.
async function applyLocally(level: number): Promise<void> {
	current = level;
	if (!isTauri()) return;
	try {
		const { getCurrentWebview } = await import('@tauri-apps/api/webview');
		await getCurrentWebview().setZoom(level);
	} catch (err) {
		const msg = err instanceof Error ? err.message : String(err);
		if (/not allowed|forbidden|permission/i.test(msg)) {
			console.error(
				'[zoom] setZoom denied by the Tauri ACL — add "core:webview:allow-set-webview-zoom" ' +
					'to src-tauri/capabilities/default.json (and window-detached.json for pop-outs).',
				err
			);
		} else {
			console.warn('[zoom] setZoom unavailable (non-Tauri context?)', err);
		}
	}
}

/**
 * Set the app zoom level: apply here, persist, and tell every other window.
 * `level` is snapped to the nearest ladder rung.
 */
export async function setZoom(level: number): Promise<void> {
	const next = clampToLadder(level);
	if (next === current) return;
	await applyLocally(next);
	try {
		localStorage.setItem(STORAGE_KEY, String(next));
	} catch {
		// Private-mode / quota failure: the zoom still applied for this
		// session, it just won't survive a restart. Not worth surfacing.
	}
	if (isTauri()) {
		try {
			const { emit } = await import('@tauri-apps/api/event');
			await emit(ZOOM_EVENT, next);
		} catch {
			// Single-window or non-Tauri: nothing to notify.
		}
	}
}

function step(direction: 1 | -1): void {
	const index = LEVELS.indexOf(clampToLadder(current));
	const nextIndex = Math.min(LEVELS.length - 1, Math.max(0, index + direction));
	void setZoom(LEVELS[nextIndex]);
}

export function zoomIn(): void {
	step(1);
}

export function zoomOut(): void {
	step(-1);
}

export function zoomReset(): void {
	void setZoom(DEFAULT_LEVEL);
}

/**
 * Boot-time install: restore the persisted level for this webview, bind the
 * hotkeys, and follow changes made in sibling windows. Returns a teardown fn.
 *
 * Called from `main.tsx` so it covers the primary workspace and every
 * detached surface window on the same code path.
 */
export function installZoom(): () => void {
	void applyLocally(readStored());

	function onKey(e: KeyboardEvent): void {
		// Zoom is deliberately NOT suppressed inside inputs/editors — unlike
		// ⌘T/⌘W, "make everything bigger" is meaningful while typing, and no
		// text field wants ⌘+ for itself.
		const mod = e.metaKey || e.ctrlKey;
		if (!mod || e.altKey) return;

		// `e.key` for the zoom-in chord varies by layout and shift state:
		// '=' unshifted on US, '+' when shifted, and some layouts report the
		// numpad as 'Add'. Match the whole family rather than just '+', which
		// is why ⌃+ silently does nothing in a lot of Electron apps.
		if (e.key === '=' || e.key === '+' || e.key === 'Add') {
			e.preventDefault();
			zoomIn();
			return;
		}
		if (e.key === '-' || e.key === '_' || e.key === 'Subtract') {
			e.preventDefault();
			zoomOut();
			return;
		}
		if (e.key === '0') {
			e.preventDefault();
			zoomReset();
		}
	}

	window.addEventListener('keydown', onKey);

	// Follow sibling windows. `emit` is broadcast-to-all including the sender,
	// so guard on the value to avoid re-applying our own change.
	let unlistenFn: (() => void) | null = null;
	let disposed = false;
	void listen<number>(ZOOM_EVENT, (event) => {
		const level = typeof event.payload === 'number' ? event.payload : DEFAULT_LEVEL;
		if (level !== current) void applyLocally(clampToLadder(level));
	})
		.then((un) => {
			// The window may have torn down while `listen` was in flight.
			if (disposed) un();
			else unlistenFn = un;
		})
		.catch(() => {
			// Non-Tauri context — hotkeys still work, just no cross-window sync.
		});

	return () => {
		disposed = true;
		window.removeEventListener('keydown', onKey);
		unlistenFn?.();
	};
}
