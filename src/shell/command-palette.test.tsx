import { act, renderHook } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { useCommandPalette } from './command-palette';

// D3 (real listener, not just the useKey() mechanism): the palette's own
// ⌘K handler is bespoke rather than routed through `useKey()` (see the
// header comment in `src/lib/keymap/defaults.ts`) because closing it must
// bypass the `not-input` guard that opening it observes — the palette's own
// cmdk search input is itself a typing target. These tests exercise that
// real listener directly.

function fireKeydown(init: KeyboardEventInit, target: EventTarget) {
	const event = new KeyboardEvent('keydown', { ...init, cancelable: true, bubbles: true });
	target.dispatchEvent(event);
	return event;
}

const MOD_K: KeyboardEventInit = { key: 'k', metaKey: true, ctrlKey: true };

describe('useCommandPalette() — ⌘K listener', () => {
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
