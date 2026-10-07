/**
 * Should a terminal take DOM focus when its xterm mounts (or its spawned PTY
 * comes up)?
 *
 * DOM focus is not cosmetic here: pane.tsx's `onFocusCapture` turns any focus
 * landing inside a pane into `focusPane(thatPane)`. So a terminal that focuses
 * itself while its pane is NOT the focused one moves pane focus with it.
 *
 * - Cache-hit re-parent: only when the hosting pane is the focused one
 *   (unchanged).
 * - Fresh mount, desktop: always, matching prior behavior.
 * - Fresh mount, remote web session: only when the hosting pane is the
 *   focused one (or there is no pane to defer to — `hostFocused` undefined,
 *   e.g. a detached surface). A restored layout mounts every live terminal at
 *   once, after hydrate; focusing each of them moved pane focus to whichever
 *   attached last, off the saved focused pane — and off a boot deep link
 *   (url-sync.ts), snapping the address bar to `/`. In a browser tab the
 *   address bar shows the focused pane, so that steal is visible.
 */
export function shouldFocusTerminalOnMount(opts: {
	/** Re-parenting an already-live cached terminal, not a fresh xterm. */
	reparent: boolean;
	/** Is the hosting pane the focused one? `undefined` = no hosting pane. */
	hostFocused: boolean | undefined;
	/** `isRemoteWebSession()`. */
	remoteWeb: boolean;
}): boolean {
	if (opts.reparent) return opts.hostFocused === true;
	if (!opts.remoteWeb) return true;
	return opts.hostFocused !== false;
}
