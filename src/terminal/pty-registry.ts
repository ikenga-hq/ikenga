import type { Pty } from './pty-bridge';
import { detachCapture } from './pty-output-buffer';

// Module-level PTY registry keyed by terminal session id (= terminal-store
// tab id, also used as pane view sessionId). Lives outside React so PTY
// instances survive component remounts when the surrounding pane tree
// rebuilds.
const registry = new Map<string, Pty>();

// Opens that have started but not resolved yet, keyed the same way. Without
// this, every caller that sees "no PTY registered" starts its own: on app
// restart the rehydrate auto-resume and each SingleTerminal effect re-run
// (the effect depends on the tab object, which every store write replaces)
// all spawned, and all but one were killed moments later with exit code 1.
const inflight = new Map<string, Promise<Pty>>();

// Sessions disposed while their open was still in flight. The PTY that lands
// afterwards is killed instead of registered, so closing a tab mid-spawn
// cannot leave an orphan process behind.
const disposedWhilePending = new Set<string>();

export function registerPty(sessionId: string, pty: Pty): void {
	registry.set(sessionId, pty);
}

export function getPty(sessionId: string): Pty | undefined {
	return registry.get(sessionId);
}

/**
 * Single-flight get-or-open for a session's PTY. Returns the registered PTY if
 * there is one, joins the in-flight open if one is running, and otherwise calls
 * `open` exactly once. `onOpened` runs once per PTY, right after registration,
 * so lifecycle wiring (exit handling, capture) cannot be attached twice.
 */
export function acquirePty(
	sessionId: string,
	open: () => Promise<Pty>,
	onOpened?: (pty: Pty) => void
): Promise<Pty> {
	const live = registry.get(sessionId);
	if (live) return Promise.resolve(live);
	const pending = inflight.get(sessionId);
	if (pending) return pending;

	disposedWhilePending.delete(sessionId);
	const opening = open()
		.then(async (pty) => {
			if (disposedWhilePending.delete(sessionId)) {
				await pty.dispose().catch(() => {});
				throw new Error(`terminal ${sessionId} was disposed while its PTY was opening`);
			}
			registry.set(sessionId, pty);
			onOpened?.(pty);
			return pty;
		})
		.finally(() => {
			inflight.delete(sessionId);
		});
	inflight.set(sessionId, opening);
	return opening;
}

export function disposePty(sessionId: string): void {
	if (inflight.has(sessionId)) disposedWhilePending.add(sessionId);
	const pty = registry.get(sessionId);
	registry.delete(sessionId);
	detachCapture(sessionId);
	if (pty) {
		pty.dispose().catch(() => {});
	}
}

export function listPtyIds(): string[] {
	return Array.from(registry.keys());
}
