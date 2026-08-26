/**
 * Terminal Keybinding Engine (T-11)
 *
 * Provides configurable action-to-chord mappings for terminal keystrokes,
 * with platform-aware defaults (macOS Cmd-based chords vs Windows/Linux
 * Ctrl+Shift chords to avoid clashing with standard PTY control codes like SIGINT).
 */

export type TerminalAction = 'copy' | 'paste' | 'find' | 'clear' | 'selectAll';

export interface TerminalKeybindings {
	copy: string;
	paste: string;
	find: string;
	clear: string;
	selectAll: string;
}

export const DEFAULT_MAC_KEYBINDINGS: TerminalKeybindings = {
	copy: 'Cmd+C',
	paste: 'Cmd+V',
	find: 'Cmd+F',
	clear: 'Cmd+K',
	selectAll: 'Cmd+A',
};

export const DEFAULT_LINUX_WIN_KEYBINDINGS: TerminalKeybindings = {
	copy: 'Ctrl+Shift+C',
	paste: 'Ctrl+Shift+V',
	find: 'Ctrl+Shift+F',
	clear: 'Ctrl+Shift+K',
	selectAll: 'Ctrl+Shift+A',
};

export function getDefaultKeybindings(isMac: boolean): TerminalKeybindings {
	return isMac ? DEFAULT_MAC_KEYBINDINGS : DEFAULT_LINUX_WIN_KEYBINDINGS;
}

/**
 * Checks if a native KeyboardEvent matches a string chord specification like
 * "Ctrl+Shift+C", "Cmd+V", "Ctrl+L", etc.
 */
export function matchesChord(e: KeyboardEvent, chord: string, isMac: boolean): boolean {
	const parts = chord.split('+').map((p) => p.trim().toLowerCase());
	if (parts.length === 0) return false;

	const targetKey = parts[parts.length - 1];
	const modifiers = new Set(parts.slice(0, parts.length - 1));

	const eventKey = e.key.toLowerCase();
	if (eventKey !== targetKey) return false;

	const requiresShift = modifiers.has('shift');
	const requiresAlt = modifiers.has('alt') || modifiers.has('option');
	const requiresCtrl = modifiers.has('ctrl') || modifiers.has('control');
	const requiresMeta = modifiers.has('meta') || modifiers.has('super');
	const requiresCmd = modifiers.has('cmd') || modifiers.has('command');

	// Shift
	if (e.shiftKey !== requiresShift) return false;

	// Alt / Option
	if (e.altKey !== requiresAlt) return false;

	// Cmd modifier resolves to metaKey on macOS and ctrlKey on Windows/Linux
	if (requiresCmd) {
		if (isMac) {
			if (!e.metaKey) return false;
		} else {
			if (!e.ctrlKey) return false;
		}
	} else {
		if (requiresMeta && !e.metaKey) return false;
		if (!requiresMeta && e.metaKey) return false;

		if (requiresCtrl && !e.ctrlKey) return false;
		if (!requiresCtrl && e.ctrlKey) return false;
	}

	return true;
}

/**
 * Evaluates a KeyboardEvent against configured keybindings and returns the
 * matching action if any.
 */
export function evaluateTerminalKey(
	e: KeyboardEvent,
	isMac: boolean,
	customConfig?: Partial<TerminalKeybindings>
): TerminalAction | null {
	if (e.type !== 'keydown') return null;

	const effective = {
		...getDefaultKeybindings(isMac),
		...customConfig,
	};

	const actions: TerminalAction[] = ['copy', 'paste', 'find', 'clear', 'selectAll'];
	for (const action of actions) {
		const chord = effective[action];
		if (chord && matchesChord(e, chord, isMac)) {
			return action;
		}
	}

	return null;
}
