// `when` clause evaluation — the piece that keeps every frame shortcut from
// firing while the user is typing (D3), and that tells `conflicts()` a real
// clash (same key + same `when`) apart from documented precedence (same key,
// different `when` — the terminal-focus `⌘K` clear vs. the global palette is
// the shipped example, §6A.5 / §2).

/** Registry `when` clauses used in Phase 1. Only `not-input` is evaluated by
 *  a live listener today; `terminal-focus` and `global` exist so the registry
 *  can *describe* bindings owned by other subsystems (the terminal, or a
 *  future editor) without wiring a duplicate handler here. */
export type WhenClause = 'global' | 'not-input' | 'terminal-focus';

/** True when `target` is a real text-entry surface: a native `input` /
 *  `textarea`, or anything marked `contenteditable="true"`. This is the same
 *  gate `activity-bar.tsx` already applied ad hoc before this registry
 *  existed — centralised here so every consumer agrees on the definition. */
export function isTypingTarget(target: EventTarget | null): boolean {
	if (!(target instanceof HTMLElement)) return false;
	return target.matches('input, textarea, [contenteditable="true"]');
}

/**
 * Evaluate a `when` clause against the keyboard event that triggered it.
 * `terminal-focus` always evaluates false here: the terminal owns its own
 * listener (`src/terminal/keybindings.ts`, do-not-touch) and the registry
 * never fires a competing handler for it — the clause exists purely so
 * `conflicts()` can record the precedence relationship.
 */
export function evaluateWhen(when: WhenClause, e: Pick<KeyboardEvent, 'target'>): boolean {
	switch (when) {
		case 'global':
			return true;
		case 'not-input':
			return !isTypingTarget(e.target as EventTarget | null);
		case 'terminal-focus':
			return false;
		default:
			return true;
	}
}
