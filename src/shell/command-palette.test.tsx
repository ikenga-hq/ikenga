import { act, renderHook } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { isMacPlatform } from '@/lib/keymap/platform';
import { useCommandPalette } from './command-palette';

// D3 through the real dispatcher (WP-54): ⌘K is two registry commands with
// mutually exclusive `when`s — `palette.open` (`!inputFocus && !paletteOpen`)
// and `palette.close` (`paletteOpen`, G-ACTIONS §4.6) — so opening observes
// the typing guard while closing still works from the palette's own cmdk
// search input, which is itself a typing target. `useCommandPalette()`
// registers both and installs the one window listener these events reach.

function fireKeydown(init: KeyboardEventInit, target: EventTarget) {
	const event = new KeyboardEvent('keydown', { ...init, cancelable: true, bubbles: true });
	target.dispatchEvent(event);
	return event;
}

// `mod+k`: ⌘K on macOS, Ctrl+K elsewhere (jsdom is not macOS). The event
// carries exactly that modifier — the registry matches the exact stroke.
const MOD_K: KeyboardEventInit = isMacPlatform() ? { key: 'k', metaKey: true } : { key: 'k', ctrlKey: true };

describe('useCommandPalette() — ⌘K through the dispatcher', () => {
	it('does not open while typing in an <input>', () => {
		const { result } = renderHook(() => useCommandPalette());
		const input = document.createElement('input');
		document.body.appendChild(input);
		act(() => {
			fireKeydown(MOD_K, input);
		});
		expect(result.current.open).toBe(false);
		input.remove();
	});

	it('does not open while typing in a <textarea>', () => {
		const { result } = renderHook(() => useCommandPalette());
		const textarea = document.createElement('textarea');
		document.body.appendChild(textarea);
		act(() => {
			fireKeydown(MOD_K, textarea);
		});
		expect(result.current.open).toBe(false);
		textarea.remove();
	});

	it('does not open while typing in a contenteditable element', () => {
		const { result } = renderHook(() => useCommandPalette());
		const div = document.createElement('div');
		div.setAttribute('contenteditable', 'true');
		document.body.appendChild(div);
		act(() => {
			fireKeydown(MOD_K, div);
		});
		expect(result.current.open).toBe(false);
		div.remove();
	});

	it('opens for a non-typing target', () => {
		const { result } = renderHook(() => useCommandPalette());
		act(() => {
			fireKeydown(MOD_K, document.body);
		});
		expect(result.current.open).toBe(true);
	});

	it('still closes via ⌘K from inside its own (typing-target) search input', () => {
		// Regression for the verifier-flagged behaviour break: guarding the
		// *open* transition against typing targets must not also guard the
		// *close* transition, or ⌘K stops closing the palette from its own
		// cmdk input — a keyboard trap the shipped app never had.
		const { result } = renderHook(() => useCommandPalette());
		act(() => {
			fireKeydown(MOD_K, document.body);
		});
		expect(result.current.open).toBe(true);

		const input = document.createElement('input');
		document.body.appendChild(input);
		act(() => {
			fireKeydown(MOD_K, input);
		});
		expect(result.current.open).toBe(false);
		input.remove();
	});
});
