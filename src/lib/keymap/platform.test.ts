import { describe, expect, it } from 'vitest';
import {
	canonicalizeKeySequence,
	eventMatchesCombo,
	formatKeyLabel,
	isChordSequence,
	type KeyEventLike,
	resolveCombo,
	resolveKeySequence,
	splitKeySequence,
	strokesFromEvent,
	toAccelerator,
	validateKeySequence,
} from './platform';

function ev(init: Partial<KeyEventLike> & { key: string }): KeyEventLike {
	return { metaKey: false, ctrlKey: false, shiftKey: false, altKey: false, ...init };
}

describe('key grammar (§3.1)', () => {
	it.each([
		'mod+k',
		'mod+shift+e',
		'mod+k mod+r',
		'ctrl+1',
		'meta+space',
		'ctrl+alt+shift+s',
		'mod+\\',
		'mod+shift+[',
		'?',
		'enter',
		'mod+plus',
		'f12',
		'alt+arrowleft',
		'shift+mod+k', // any modifier order is accepted
	])('accepts %j', (k) => {
		expect(validateKeySequence(k)).toBeNull();
	});

	it.each([
		'Mod+K', // lowercase only
		'mod+k mod+r mod+x', // at most two strokes
		'mod+k  mod+r', // exactly one space
		' mod+k',
		'mod+mod+k',
		'mod+ctrl+k',
		'mod+meta+k',
		'hyper+k',
		'mod+',
		'mod+{', // shifted punctuation names the unshifted key
		'mod+f25',
		'',
	])('rejects %j', (k) => {
		expect(validateKeySequence(k)).not.toBeNull();
	});

	it('canonicalizes modifier order to mod, ctrl, meta, alt, shift', () => {
		expect(canonicalizeKeySequence('shift+mod+k')).toBe('mod+shift+k');
		expect(canonicalizeKeySequence('shift+alt+ctrl+s')).toBe('ctrl+alt+shift+s');
		expect(canonicalizeKeySequence('shift+mod+k alt+mod+r')).toBe('mod+shift+k mod+alt+r');
	});

	it('splits and detects chords', () => {
		expect(splitKeySequence('mod+k mod+r')).toEqual(['mod+k', 'mod+r']);
		expect(isChordSequence('mod+k mod+r')).toBe(true);
		expect(isChordSequence('mod+k')).toBe(false);
	});

	it('resolves mod per platform, stroke by stroke', () => {
		expect(resolveCombo('mod+shift+k', true)).toBe('meta+shift+k');
		expect(resolveCombo('mod+shift+k', false)).toBe('ctrl+shift+k');
		expect(resolveCombo('ctrl+1', true)).toBe('ctrl+1');
		expect(resolveKeySequence('mod+k mod+r', true)).toBe('meta+k meta+r');
		expect(resolveKeySequence('mod+k mod+r', false)).toBe('ctrl+k ctrl+r');
	});
});

describe('strokesFromEvent — keyboard robustness (§3.3)', () => {
	it('never matches during IME composition', () => {
		expect(strokesFromEvent(ev({ key: 'k', metaKey: true, isComposing: true }))).toEqual([]);
		expect(strokesFromEvent(ev({ key: 'Process', keyCode: 229 }))).toEqual([]);
		expect(strokesFromEvent(ev({ key: 'k', keyCode: 229 }))).toEqual([]);
		expect(eventMatchesCombo(ev({ key: 'k', metaKey: true, isComposing: true }), 'mod+k', true)).toBe(false);
	});

	it('never matches a Dead key', () => {
		expect(strokesFromEvent(ev({ key: 'Dead', code: 'Quote' }))).toEqual([]);
		expect(strokesFromEvent(ev({ key: 'Dead', code: 'KeyE', altKey: true }))).toEqual([]);
	});

	it('ignores a bare modifier press', () => {
		expect(strokesFromEvent(ev({ key: 'Shift', shiftKey: true }))).toEqual([]);
		expect(strokesFromEvent(ev({ key: 'Meta', metaKey: true }))).toEqual([]);
	});
});

describe('strokesFromEvent — the key-vs-code choice', () => {
	it('uses the produced letter/digit (layout-labelled)', () => {
		expect(strokesFromEvent(ev({ key: 'k', metaKey: true, code: 'KeyK' }))).toEqual(['meta+k']);
		expect(strokesFromEvent(ev({ key: 'N', metaKey: true, shiftKey: true, code: 'KeyN' }))).toEqual(['meta+shift+n']);
		// AZERTY: the key labelled Z sits at KeyW — ⌘Z is the key that says Z.
		expect(strokesFromEvent(ev({ key: 'z', metaKey: true, code: 'KeyW' }))).toEqual(['meta+z']);
	});

	it('falls back to the physical position for shifted punctuation', () => {
		// ⌘⇧[ arrives as `{` on US — matched by code as `mod+shift+[`.
		const e = ev({ key: '{', metaKey: true, shiftKey: true, code: 'BracketLeft' });
		expect(strokesFromEvent(e)).toEqual(['meta+shift+[']);
		expect(eventMatchesCombo(e, 'mod+shift+[', true)).toBe(true);
		expect(eventMatchesCombo(ev({ key: '|', ctrlKey: true, shiftKey: true, code: 'Backslash' }), 'mod+shift+\\', false)).toBe(true);
	});

	it('falls back to the position for Alt-transformed characters and non-Latin layouts', () => {
		expect(strokesFromEvent(ev({ key: '¡', altKey: true, code: 'Digit1' }))).toEqual(['alt+1']);
		expect(strokesFromEvent(ev({ key: 'å', altKey: true, metaKey: true, code: 'KeyA' }))).toEqual(['meta+alt+a']);
		expect(strokesFromEvent(ev({ key: 'с', metaKey: true, code: 'KeyC' }))).toEqual(['meta+c']);
		// AZERTY digit row: unshifted produces `&`, the position is Digit1.
		expect(strokesFromEvent(ev({ key: '&', ctrlKey: true, code: 'Digit1' }))).toEqual(['ctrl+1']);
	});

	it('uses the produced character for unshifted grammar punctuation', () => {
		expect(strokesFromEvent(ev({ key: ',', metaKey: true, code: 'Comma' }))).toEqual(['meta+,']);
		expect(strokesFromEvent(ev({ key: '\\', metaKey: true, code: 'Backslash' }))).toEqual(['meta+\\']);
		expect(strokesFromEvent(ev({ key: '/', metaKey: true }))).toEqual(['meta+/']);
	});

	it('matches `?` on the produced character with Shift ignored', () => {
		const us = ev({ key: '?', shiftKey: true, code: 'Slash' });
		expect(strokesFromEvent(us)).toEqual(['?', 'shift+/']);
		expect(eventMatchesCombo(us, '?', true)).toBe(true);
		// A layout where `?` is unshifted.
		expect(eventMatchesCombo(ev({ key: '?', code: 'Minus' }), '?', false)).toBe(true);
	});

	it('matches `plus` on the produced character with Shift ignored', () => {
		const us = ev({ key: '+', metaKey: true, shiftKey: true, code: 'Equal' });
		expect(eventMatchesCombo(us, 'mod+plus', true)).toBe(true);
		expect(eventMatchesCombo(us, 'mod+shift+=', true)).toBe(true);
		expect(eventMatchesCombo(ev({ key: '+', ctrlKey: true, code: 'NumpadAdd' }), 'mod+plus', false)).toBe(true);
	});

	it('names keys by their grammar name', () => {
		expect(strokesFromEvent(ev({ key: ' ', altKey: true, code: 'Space' }))).toEqual(['alt+space']);
		expect(strokesFromEvent(ev({ key: 'Enter', shiftKey: true }))).toEqual(['shift+enter']);
		expect(strokesFromEvent(ev({ key: 'ArrowUp', metaKey: true }))).toEqual(['meta+arrowup']);
		expect(strokesFromEvent(ev({ key: 'Escape' }))).toEqual(['escape']);
		expect(strokesFromEvent(ev({ key: 'F5' }))).toEqual(['f5']);
	});

	it('requires the exact modifier set', () => {
		expect(eventMatchesCombo(ev({ key: 'k', metaKey: true, shiftKey: true }), 'mod+k', true)).toBe(false);
		expect(eventMatchesCombo(ev({ key: 'k', ctrlKey: true }), 'mod+k', true)).toBe(false);
		expect(eventMatchesCombo(ev({ key: 'k', ctrlKey: true }), 'mod+k', false)).toBe(true);
	});

	it('never matches a chord against a single event', () => {
		expect(eventMatchesCombo(ev({ key: 'k', metaKey: true }), 'mod+k mod+r', true)).toBe(false);
	});
});

describe('display', () => {
	it('formats chords stroke by stroke', () => {
		expect(formatKeyLabel('mod+k mod+r', { mac: true })).toBe('⌘K ⌘R');
		expect(formatKeyLabel('mod+k mod+r', { mac: false })).toBe('Ctrl+K Ctrl+R');
	});

	it('keeps the shipped glyph forms', () => {
		expect(formatKeyLabel('mod+shift+\\', { mac: true })).toBe('⌘⇧\\');
		expect(formatKeyLabel('ctrl+t', { mac: true })).toBe('⌃T');
		expect(formatKeyLabel('mod+1', { mac: false })).toBe('Ctrl+1');
		expect(formatKeyLabel('alt+arrowleft', { mac: true })).toBe('⌥←');
	});

	it('emits Tauri accelerators, none for a chord', () => {
		expect(toAccelerator('mod+shift+o')).toBe('CmdOrCtrl+Shift+O');
		expect(toAccelerator('mod+alt+arrowup')).toBe('CmdOrCtrl+Alt+Up');
		expect(toAccelerator('mod+plus')).toBe('CmdOrCtrl+Plus');
		expect(toAccelerator('mod+k mod+r')).toBeUndefined();
	});
});
