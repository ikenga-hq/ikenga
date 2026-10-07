import { describe, expect, it } from 'vitest';
import { shouldFocusTerminalOnMount as focus } from './mount-focus';

describe('shouldFocusTerminalOnMount', () => {
	it('desktop: unchanged — a fresh mount always focuses, a re-parent only when focused', () => {
		for (const hostFocused of [true, false, undefined]) {
			expect(focus({ reparent: false, hostFocused, remoteWeb: false })).toBe(true);
		}
		expect(focus({ reparent: true, hostFocused: true, remoteWeb: false })).toBe(true);
		expect(focus({ reparent: true, hostFocused: false, remoteWeb: false })).toBe(false);
		expect(focus({ reparent: true, hostFocused: undefined, remoteWeb: false })).toBe(false);
	});

	it('remote web: a restored terminal in an unfocused pane does not take focus', () => {
		// The boot deep-link case: pane focus (and with it the address bar)
		// must stay on the saved / deep-linked route pane.
		expect(focus({ reparent: false, hostFocused: false, remoteWeb: true })).toBe(false);
	});

	it('remote web: a terminal in the focused pane, or with no hosting pane, still focuses', () => {
		expect(focus({ reparent: false, hostFocused: true, remoteWeb: true })).toBe(true);
		expect(focus({ reparent: false, hostFocused: undefined, remoteWeb: true })).toBe(true);
		expect(focus({ reparent: true, hostFocused: true, remoteWeb: true })).toBe(true);
		expect(focus({ reparent: true, hostFocused: false, remoteWeb: true })).toBe(false);
	});
});
