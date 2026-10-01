/**
 * Re-send this viewer's terminal size to the PTY when its window regains focus.
 *
 * `Pty.resize` deliberately drops the call for a non-owning (attached) viewer whose window is
 * not focused (D-10, active-viewer priority) so two windows on the same PTY cannot fight over
 * its size. The cost: while the window is unfocused it never reports a size change, so the PTY
 * stays at whatever the *other* viewer last set. Coming back to the window nothing corrects it
 * (xterm's own `onResize` only fires when its grid changes), and the child process keeps
 * drawing at the wrong width, which reads as garbled output until something forces a redraw.
 *
 * Fix: on window focus, push the size this terminal already has. If the PTY is already that
 * size the kernel drops the identical winsize (no SIGWINCH, nothing to do). If another viewer
 * had changed it, this is a real change and a full-screen TUI repaints at the right size.
 *
 * Returns a disposer that removes the listener.
 */
export function resyncPtySizeOnWindowFocus(
	pty: { resize(rows: number, cols: number): Promise<void> },
	term: { rows: number; cols: number },
	win: Pick<Window, 'addEventListener' | 'removeEventListener'> | undefined = typeof window ===
	'undefined'
		? undefined
		: window
): () => void {
	if (!win) return () => undefined;
	const onFocus = () => {
		pty.resize(term.rows, term.cols).catch(() => {});
	};
	win.addEventListener('focus', onFocus);
	return () => win.removeEventListener('focus', onFocus);
}
