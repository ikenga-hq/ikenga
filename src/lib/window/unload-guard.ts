// Accidental-close guard for a browser tab.
//
// A terminal's readline keys (Ctrl+W delete-word, Ctrl+T, Ctrl+N, Ctrl+Q) are
// browser shortcuts too, and a page cannot stop them: Ctrl+W in a focused
// xterm closes the whole Ikenga tab instead of deleting a word. The only
// thing a page can do is ask before it goes, so in a browser session the
// guard registers a `beforeunload` handler while any terminal tab exists
// (which includes a live agent session — an agent runs in a terminal tab).
//
// The handler is attached only while it can matter: never on the desktop
// (Tauri has its own close flow), and in a browser only from the first
// terminal tab until the last one closes, so an idle tab stays bfcache- and
// quit-friendly.

import { isBrowserSession } from '@/lib/transport';
import { useTerminalStore } from '@/terminal/session-store';

/** The `beforeunload` handler: standard "ask before leaving" prompt. */
export function beforeUnloadPrompt(e: BeforeUnloadEvent): string {
	e.preventDefault();
	// Legacy browsers read `returnValue`; modern ones show their own text.
	e.returnValue = '';
	return '';
}

export interface UnloadGuardDeps {
	/** Is this a browser tab on a daemon-served page? */
	isBrowser?: () => boolean;
	/** Number of open terminal tabs now. */
	terminalCount?: () => number;
	/** Subscribe to terminal-tab changes; returns an unsubscribe. */
	subscribe?: (listener: () => void) => () => void;
	target?: Pick<Window, 'addEventListener' | 'removeEventListener'>;
}

/**
 * Install the guard. Returns a teardown. A no-op (returns a no-op teardown)
 * outside a browser session.
 */
export function installUnloadGuard(deps: UnloadGuardDeps = {}): () => void {
	const isBrowser = deps.isBrowser ?? isBrowserSession;
	if (!isBrowser()) return () => {};
	const count = deps.terminalCount ?? (() => useTerminalStore.getState().tabs.length);
	const subscribe = deps.subscribe ?? ((listener) => useTerminalStore.subscribe(listener));
	const target = deps.target ?? window;

	let attached = false;
	const sync = () => {
		const want = count() > 0;
		if (want === attached) return;
		attached = want;
		if (want) target.addEventListener('beforeunload', beforeUnloadPrompt);
		else target.removeEventListener('beforeunload', beforeUnloadPrompt);
	};
	sync();
	const unsubscribe = subscribe(sync);
	return () => {
		unsubscribe();
		if (attached) target.removeEventListener('beforeunload', beforeUnloadPrompt);
		attached = false;
	};
}
