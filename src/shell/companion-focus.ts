// Companion focus seam for the rail's Chi key (⌘2) — WP-03 stub.
//
// The Chi key must bring the Companion forward (spec §2 ⌘2, §3.1 row 2), but
// the Companion itself is WP-06, built in parallel. Rather than import a store
// that does not exist yet, the rail announces the intent on `window` and the
// Companion listens: WP-06 subscribes to `COMPANION_FOCUS_EVENT` and expands /
// focuses itself. Until then nothing listens and the dispatch is a no-op.
//
// Keep this module dependency-free so both sides can import it without
// creating a cycle through the frame.

export const COMPANION_FOCUS_EVENT = 'ikenga:companion-focus';

/** Ask the Companion to expand and take focus. Safe with no listener and
 *  outside a browser (tests, SSR). */
export function focusCompanion(): void {
	if (typeof window === 'undefined') return;
	window.dispatchEvent(new CustomEvent(COMPANION_FOCUS_EVENT));
}
