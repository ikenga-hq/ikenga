/**
 * Terminal keybindings (T-11) — read from the registry (WP-54, DEC-56).
 *
 * The terminal's actions are the hosted `terminal.*` commands of the one
 * keymap (G-ACTIONS §4.6, §10.2): `defaults.ts` holds their platform defaults
 * (⌘-based on macOS; Ctrl+Shift-based on Windows/Linux, so plain Ctrl+C /
 * Ctrl+V stay SIGINT / literal for the PTY), and a personal or project
 * `keybindings.json` rule rebinds them like any other key — one grammar, no
 * private chord table. The xterm hook stays their owner: it asks
 * `evaluateTerminalKey()` which action a keydown is, and fires it itself; the
 * frame dispatcher never does. The terminal still owns ⌘K while it has focus
 * (`terminal.clear`, DEC-57).
 */

import { type ContextKeys, getContextKeys, getEvalOptions } from '@/lib/keymap/context-keys';
import type { KeymapEntry } from '@/lib/keymap/defaults';
import { eventMatchesCombo, isMacPlatform } from '@/lib/keymap/platform';
import { entriesForPlatform, getKeymap, labelFor, resolveKeypressWinner } from '@/lib/keymap/registry';
import { evaluateWhen, type WhenContext } from '@/lib/keymap/when';

export type TerminalAction =
	| 'copy'
	| 'paste'
	| 'find'
	| 'clear'
	| 'selectAll'
	| 'jumpToPrevPrompt'
	| 'jumpToNextPrompt';

/** Terminal action → its registry command (§10.2). */
export const TERMINAL_COMMANDS: Readonly<Record<TerminalAction, string>> = {
	copy: 'terminal.copy',
	paste: 'terminal.paste',
	find: 'terminal.find',
	clear: 'terminal.clear',
	selectAll: 'terminal.select-all',
	jumpToPrevPrompt: 'terminal.prev-prompt',
	jumpToNextPrompt: 'terminal.next-prompt',
};

const ACTION_BY_COMMAND: ReadonlyMap<string, TerminalAction> = new Map(
	(Object.entries(TERMINAL_COMMANDS) as Array<[TerminalAction, string]>).map(([action, command]) => [command, action])
);

/** The terminal action a registry command names, or null. */
export function terminalActionFor(command: string): TerminalAction | null {
	return ACTION_BY_COMMAND.get(command) ?? null;
}

export interface TerminalKeyOptions {
	/** The effective keymap (default: `getKeymap()`). */
	entries?: readonly KeymapEntry[];
	/** Default: the live platform. */
	mac?: boolean;
	/** Context the `when`s evaluate against (default: the live context of the
	 *  event's target — inside the xterm host, so `terminalFocus` holds). */
	ctx?: ContextKeys | WhenContext;
}

/**
 * The terminal action a keydown fires, or null: the §2.3 winner among the
 * effective `terminal.*` rules whose key matches and whose `when` holds.
 * IME composition and Dead keys never match (`strokesFromEvent`).
 */
export function evaluateTerminalKey(e: KeyboardEvent, opts: TerminalKeyOptions = {}): TerminalAction | null {
	if (e.type !== 'keydown') return null;
	const mac = opts.mac ?? isMacPlatform();
	const entries = opts.entries ?? getKeymap();
	const ctx = opts.ctx ?? getContextKeys(e.target);
	const evalOpts = getEvalOptions();
	const hits = entriesForPlatform(entries, mac ? 'mac' : 'other').filter(
		(entry) =>
			(entry.scope ?? 'app') === 'app' &&
			terminalActionFor(entry.command) !== null &&
			eventMatchesCombo(e, entry.key, mac) &&
			evaluateWhen(entry.when, ctx, evalOpts)
	);
	const winner = hits.length > 0 ? resolveKeypressWinner(hits, entries) : null;
	return winner ? terminalActionFor(winner.command) : null;
}

/** The key hint for a terminal action (context menu), from the registry. */
export function terminalKeyLabel(action: TerminalAction, opts?: { mac?: boolean }): string {
	return labelFor(TERMINAL_COMMANDS[action], opts);
}
