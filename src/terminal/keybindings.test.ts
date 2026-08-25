import { describe, expect, it } from 'vitest';
import {
	DEFAULT_LINUX_WIN_KEYBINDINGS,
	DEFAULT_MAC_KEYBINDINGS,
	evaluateTerminalKey,
	getDefaultKeybindings,
	matchesChord,
} from './keybindings';

describe('terminal keybindings', () => {
	it('exports valid platform default constants', () => {
		expect(DEFAULT_MAC_KEYBINDINGS.copy).toBe('Cmd+C');
		expect(DEFAULT_LINUX_WIN_KEYBINDINGS.copy).toBe('Ctrl+Shift+C');
		expect(getDefaultKeybindings(true)).toEqual(DEFAULT_MAC_KEYBINDINGS);
		expect(getDefaultKeybindings(false)).toEqual(DEFAULT_LINUX_WIN_KEYBINDINGS);
	});

	it('matches Mac chords correctly', () => {
		const cmdC = new KeyboardEvent('keydown', { key: 'c', metaKey: true });
		expect(matchesChord(cmdC, 'Cmd+C', true)).toBe(true);
		expect(matchesChord(cmdC, 'Ctrl+Shift+C', true)).toBe(false);

		const cmdV = new KeyboardEvent('keydown', { key: 'v', metaKey: true });
		expect(matchesChord(cmdV, 'Cmd+V', true)).toBe(true);

		const cmdShiftF = new KeyboardEvent('keydown', { key: 'f', metaKey: true, shiftKey: true });
		expect(matchesChord(cmdShiftF, 'Cmd+Shift+F', true)).toBe(true);
	});

	it('matches Linux/Windows chords correctly', () => {
		const ctrlShiftC = new KeyboardEvent('keydown', { key: 'c', ctrlKey: true, shiftKey: true });
		expect(matchesChord(ctrlShiftC, 'Ctrl+Shift+C', false)).toBe(true);
		expect(matchesChord(ctrlShiftC, 'Cmd+C', false)).toBe(false);

		const ctrlShiftV = new KeyboardEvent('keydown', { key: 'v', ctrlKey: true, shiftKey: true });
		expect(matchesChord(ctrlShiftV, 'Ctrl+Shift+V', false)).toBe(true);

		const ctrlShiftF = new KeyboardEvent('keydown', { key: 'f', ctrlKey: true, shiftKey: true });
		expect(matchesChord(ctrlShiftF, 'Ctrl+Shift+F', false)).toBe(true);
	});

	it('evaluates terminal actions with platform defaults', () => {
		const macEvent = new KeyboardEvent('keydown', { key: 'c', metaKey: true });
		expect(evaluateTerminalKey(macEvent, true)).toBe('copy');

		const winEvent = new KeyboardEvent('keydown', { key: 'v', ctrlKey: true, shiftKey: true });
		expect(evaluateTerminalKey(winEvent, false)).toBe('paste');

		const winFind = new KeyboardEvent('keydown', { key: 'f', ctrlKey: true, shiftKey: true });
		expect(evaluateTerminalKey(winFind, false)).toBe('find');

		const plainC = new KeyboardEvent('keydown', { key: 'c' });
		expect(evaluateTerminalKey(plainC, false)).toBeNull();
	});

	it('supports custom keybinding overrides', () => {
		const customEvent = new KeyboardEvent('keydown', { key: 'y', ctrlKey: true, altKey: true });
		const action = evaluateTerminalKey(customEvent, false, {
			copy: 'Ctrl+Alt+Y',
		});
		expect(action).toBe('copy');
	});
});
