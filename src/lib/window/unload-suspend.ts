// A deliberate, user-requested page reload (the PWA "Reload to update"
// hand-over) must not be stopped by the terminal "Leave site?" prompt in
// `unload-guard.ts`. That prompt exists to catch an ACCIDENTAL tab close — a
// terminal's Ctrl+W is also a browser shortcut — and in a browser session the
// terminals live on the server and reattach after a reload, so nothing is lost
// by a reload the user asked for.
//
// This lives in its own tiny module so `lib/pwa/register.ts`, which loads at
// boot, does not pull the terminal store in just to flip a flag.

let suspended = false;
const REARM_MS = 1000;

/** True while the terminal unload prompt should stand aside. */
export function isTerminalUnloadGuardSuspended(): boolean {
	return suspended;
}

/**
 * Run `fn` (a deliberate page reload) with the TERMINAL unload prompt stepped
 * aside. Only that prompt: other `beforeunload` handlers — notably the
 * unsaved-edits one in `lib/editing/editing-store.ts` — still run and still
 * prompt. The guard is re-armed right after, so a reload the user cancels at
 * the browser's prompt (or a browser that dispatches `beforeunload` a tick
 * late) leaves it in force for any later accidental close.
 */
export function withTerminalGuardSuspended(fn: () => void): void {
	suspended = true;
	try {
		fn();
	} finally {
		setTimeout(() => {
			suspended = false;
		}, REARM_MS);
	}
}

/** Test seam. */
export function _resetUnloadSuspendForTests(): void {
	suspended = false;
}
