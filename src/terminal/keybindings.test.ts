// The terminal's keys are the registry's hosted `terminal.*` commands
// (WP-54, DEC-56, G-ACTIONS §10.2): same platform defaults as the old T-11
// table, now rebindable through `keybindings.json` like any other key.

import { describe, expect, it } from 'vitest';
import { DEFAULT_KEYMAP, type KeymapEntry } from '@/lib/keymap/defaults';
import { evaluateTerminalKey, TERMINAL_COMMANDS, terminalActionFor, terminalKeyLabel } from './keybindings';

const IN_TERMINAL = { terminalFocus: true };
const key = (init: KeyboardEventInit) => new KeyboardEvent('keydown', init);

describe('terminal keybindings (registry `terminal.*`)', () => {
	it('every terminal action is a default registry command on both platforms', () => {
		for (const command of Object.values(TERMINAL_COMMANDS)) {
			const entries = DEFAULT_KEYMAP.filter((e) => e.command === command);
			expect(entries.map((e) => e.platformOnly).sort(), command).toEqual(['mac', 'other']);
			for (const e of entries) expect(e.when).toBe('terminalFocus');
			expect(terminalActionFor(command)).not.toBeNull();
		}
		expect(terminalActionFor('palette.open')).toBeNull();
	});

	it('macOS defaults are ⌘-based', () => {
		const opts = { mac: true, ctx: IN_TERMINAL };
		expect(evaluateTerminalKey(key({ key: 'c', metaKey: true }), opts)).toBe('copy');
		expect(evaluateTerminalKey(key({ key: 'v', metaKey: true }), opts)).toBe('paste');
		expect(evaluateTerminalKey(key({ key: 'f', metaKey: true }), opts)).toBe('find');
		expect(evaluateTerminalKey(key({ key: 'k', metaKey: true }), opts)).toBe('clear');
		expect(evaluateTerminalKey(key({ key: 'a', metaKey: true }), opts)).toBe('selectAll');
		expect(evaluateTerminalKey(key({ key: 'ArrowUp', metaKey: true }), opts)).toBe('jumpToPrevPrompt');
		expect(evaluateTerminalKey(key({ key: 'ArrowDown', metaKey: true }), opts)).toBe('jumpToNextPrompt');
		// Ctrl+Shift+C is not a mac binding.
		expect(evaluateTerminalKey(key({ key: 'c', ctrlKey: true, shiftKey: true }), opts)).toBeNull();
	});

	it('Windows/Linux defaults are Ctrl+Shift-based; plain Ctrl+C stays for the PTY', () => {
		const opts = { mac: false, ctx: IN_TERMINAL };
		expect(evaluateTerminalKey(key({ key: 'C', ctrlKey: true, shiftKey: true }), opts)).toBe('copy');
		expect(evaluateTerminalKey(key({ key: 'V', ctrlKey: true, shiftKey: true }), opts)).toBe('paste');
		expect(evaluateTerminalKey(key({ key: 'F', ctrlKey: true, shiftKey: true }), opts)).toBe('find');
		expect(evaluateTerminalKey(key({ key: 'K', ctrlKey: true, shiftKey: true }), opts)).toBe('clear');
		expect(evaluateTerminalKey(key({ key: 'ArrowUp', ctrlKey: true }), opts)).toBe('jumpToPrevPrompt');
		expect(evaluateTerminalKey(key({ key: 'ArrowDown', ctrlKey: true }), opts)).toBe('jumpToNextPrompt');
		expect(evaluateTerminalKey(key({ key: 'c', ctrlKey: true }), opts)).toBeNull();
		expect(evaluateTerminalKey(key({ key: 'c' }), opts)).toBeNull();
	});

	it('fires only while the terminal has focus (`when: terminalFocus`)', () => {
		expect(evaluateTerminalKey(key({ key: 'k', metaKey: true }), { mac: true, ctx: { terminalFocus: false } })).toBeNull();
	});

	it('a personal rebind changes the key (one grammar, no private table)', () => {
		const entries: KeymapEntry[] = [
			...DEFAULT_KEYMAP.filter((e) => e.command !== 'terminal.copy'),
			{ command: 'terminal.copy', key: 'ctrl+alt+y', when: 'terminalFocus', source: 'personal', label: 'Copy' },
		];
		const opts = { mac: false, entries, ctx: IN_TERMINAL };
		expect(evaluateTerminalKey(key({ key: 'y', ctrlKey: true, altKey: true }), opts)).toBe('copy');
		expect(evaluateTerminalKey(key({ key: 'C', ctrlKey: true, shiftKey: true }), opts)).toBeNull();
	});

	it('IME composition never fires', () => {
		const composing = new KeyboardEvent('keydown', { key: 'k', metaKey: true, isComposing: true });
		expect(evaluateTerminalKey(composing, { mac: true, ctx: IN_TERMINAL })).toBeNull();
	});

	it('labels come from the registry', () => {
		expect(terminalKeyLabel('copy', { mac: true })).toBe('⌘C');
		expect(terminalKeyLabel('copy', { mac: false })).toBe('Ctrl+Shift+C');
	});
});
