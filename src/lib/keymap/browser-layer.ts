// Browser platform layer for the default keymap.
//
// In a normal browser tab the browser, not the page, owns Ctrl/Cmd + W, T, N,
// Shift+T / N / W, Tab, L, R and Q: those chords never reach a page's
// `keydown`, and Ctrl+W closes the whole Ikenga tab. The desktop defaults
// (`defaults.ts`) bind several of them (`pane.close`, `pane.new-shell-terminal`,
// `ngwa.create`, ...), so in a browser session the *default layer* is remapped
// onto Alt-based chords the browser leaves alone.
//
// This is a layer over the defaults, applied before the effective merge
// (`actions/merge.ts`), so everything downstream follows with no special
// casing: `labelFor()` / `<kbd>` hints / the shortcuts overlay show the
// browser keys, `conflicts()` checks them, and a personal or project rule
// still overrides them exactly as it overrides any default. Desktop (Tauri)
// and test harnesses never take this path (`isBrowserSession()`).
//
// Zoom is not remapped: it is handled by NOT registering the `zoom.*`
// handlers in a browser (`lib/window/zoom.ts`), so the dispatcher never claims
// Ctrl/Cmd + = / - / 0 and the browser's own zoom works.

import { isBrowserSession } from '@/lib/transport';
import { DEFAULT_KEYMAP, type KeymapEntry } from './defaults';
import { parseCombo, resolveCombo } from './platform';

/** Browser-owned chords: the platform-primary modifier (plus optionally
 *  Shift) with one of these keys never reaches the page. `tab` covers
 *  Ctrl+Tab / Ctrl+Shift+Tab. */
const RESERVED_KEYS: ReadonlySet<string> = new Set(['w', 't', 'n', 'tab', 'l', 'r', 'q']);

/** Would a browser swallow this stroke on `mac` (Cmd) or elsewhere (Ctrl)? */
export function isBrowserReserved(stroke: string, mac: boolean): boolean {
	const { mods, key } = parseCombo(stroke);
	if (!RESERVED_KEYS.has(key)) return false;
	if (mods.has('alt')) return false;
	// Resolved form is `meta+…` on macOS, `ctrl+…` elsewhere; only Shift may
	// ride along with the primary modifier.
	const modifiers = resolveCombo(stroke, mac).split('+').slice(0, -1);
	const primary = mac ? 'meta' : 'ctrl';
	return modifiers.includes(primary) && modifiers.every((m) => m === primary || m === 'shift');
}

/** Whether any stroke of a (possibly two-stroke) key is browser-reserved on
 *  either platform. */
function reservedAnywhere(key: string): boolean {
	return key.split(/\s+/).some((s) => isBrowserReserved(s, true) || isBrowserReserved(s, false));
}

/**
 * Default key → browser key, per command. Alt is the one modifier no browser
 * claims for letters on all three desktop OSes (macOS Option arrives as
 * `code`, which `strokesFromEvent` already maps). Matched on the *default*
 * key too, so a command a future default binds elsewhere is left untouched.
 */
const BROWSER_REMAP: ReadonlyArray<{ command: string; from: string; to: string }> = [
	{ command: 'ngwa.create', from: 'mod+n', to: 'alt+n' },
	{ command: 'pane.new-shell-terminal', from: 'ctrl+t', to: 'alt+t' },
	{ command: 'pane.new-claude-terminal', from: 'ctrl+shift+t', to: 'alt+shift+t' },
	{ command: 'pane.new-artifact', from: 'mod+shift+n', to: 'alt+shift+n' },
	{ command: 'pane.close', from: 'mod+w', to: 'alt+w' },
	{ command: 'tab.close', from: 'mod+shift+w', to: 'alt+shift+w' },
	{ command: 'people.lock-now', from: 'mod+shift+l', to: 'alt+shift+l' },
	// macOS-only defaults on Cmd+T / Cmd+Shift+T (Cmd+T is the browser's own
	// new-tab). `alt+t` is taken by the terminal above, so views gets `alt+v`.
	{ command: 'palette.views', from: 'mod+t', to: 'alt+v' },
	{ command: 'pane.reopen', from: 'mod+shift+t', to: 'alt+shift+r' },
];

/**
 * The default keymap as a browser tab sees it: reserved chords remapped to
 * their Alt equivalents. Pure; every other entry is returned by reference.
 */
export function browserKeymap(entries: readonly KeymapEntry[]): KeymapEntry[] {
	return entries.map((entry) => {
		const hit = BROWSER_REMAP.find((r) => r.command === entry.command && r.from === entry.key);
		return hit ? { ...entry, key: hit.to } : entry;
	});
}

/** Default entries that still bind a browser-reserved chord after remapping:
 *  the list the guard test asserts is empty, so a new default on Ctrl+W
 *  cannot ship without a browser alternative. `app` rules only — OS-wide
 *  rules go through the desktop's global-shortcut plugin, never a browser. */
export function browserReservedLeftovers(entries: readonly KeymapEntry[]): KeymapEntry[] {
	return browserKeymap(entries).filter(
		(e) => (e.scope ?? 'app') === 'app' && reservedAnywhere(e.key)
	);
}

let browserCache: KeymapEntry[] | null = null;

/**
 * The default layer for this session: `DEFAULT_KEYMAP` on the desktop, its
 * browser remap in a browser tab. The one entry point the effective merge
 * and `getKeymap()`'s no-model fallback read the defaults through.
 */
export function defaultKeymap(): KeymapEntry[] {
	if (!isBrowserSession()) return DEFAULT_KEYMAP;
	browserCache ??= browserKeymap(DEFAULT_KEYMAP);
	return browserCache;
}
