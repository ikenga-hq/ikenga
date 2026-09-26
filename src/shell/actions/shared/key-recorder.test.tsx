// Item 8 (review round 1): chord recording within the DEC-57 900ms window,
// the ctrl+meta literal encoding when both platform modifiers are held at
// once, and Backspace-clears / Escape-cancels.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { KeyRecorder } from './key-recorder';

vi.mock('@/lib/platform', () => ({ isMac: false }));

afterEach(cleanup);

function press(key: string, opts: Partial<KeyboardEventInit> = {}) {
	// Dispatched on `document`, same as this repo's other global-shortcut
	// tests (`routes/ngwa/-scopes-route.test.tsx`) — `window` is an ancestor
	// of `document` in the DOM event path, so a capture-phase listener on
	// `window` (this component's) still sees it.
	fireEvent.keyDown(document, { key, ...opts });
}

describe('KeyRecorder — single stroke', () => {
	beforeEach(() => vi.useFakeTimers());
	afterEach(() => vi.useRealTimers());

	it('commits a single stroke once the chord window elapses', () => {
		const onRecord = vi.fn();
		render(<KeyRecorder onRecord={onRecord} />);
		fireEvent.click(screen.getByRole('button'));
		press('e', { ctrlKey: true });
		expect(onRecord).not.toHaveBeenCalled();
		vi.advanceTimersByTime(900);
		expect(onRecord).toHaveBeenCalledWith('mod+e');
	});
});

describe('KeyRecorder — chord (DEC-57)', () => {
	beforeEach(() => vi.useFakeTimers());
	afterEach(() => vi.useRealTimers());

	it('combines two strokes into a chord when the second arrives within 900ms', () => {
		const onRecord = vi.fn();
		render(<KeyRecorder onRecord={onRecord} />);
		fireEvent.click(screen.getByRole('button'));
		press('k', { ctrlKey: true });
		vi.advanceTimersByTime(400);
		press('r', { ctrlKey: true });
		expect(onRecord).toHaveBeenCalledWith('mod+k mod+r');
	});

	it('does not chord once the window has already elapsed', () => {
		const onRecord = vi.fn();
		render(<KeyRecorder onRecord={onRecord} />);
		fireEvent.click(screen.getByRole('button'));
		press('k', { ctrlKey: true });
		vi.advanceTimersByTime(900);
		expect(onRecord).toHaveBeenCalledWith('mod+k');
		expect(onRecord).toHaveBeenCalledTimes(1);
	});
});

describe('KeyRecorder — both platform modifiers held', () => {
	beforeEach(() => vi.useFakeTimers());
	afterEach(() => vi.useRealTimers());

	it('records the literal ctrl+meta, never mod+ctrl or mod+meta', () => {
		const onRecord = vi.fn();
		render(<KeyRecorder onRecord={onRecord} />);
		fireEvent.click(screen.getByRole('button'));
		press('t', { ctrlKey: true, metaKey: true });
		// A single stroke commits once the chord window elapses.
		vi.advanceTimersByTime(900);
		expect(onRecord).toHaveBeenCalledWith('ctrl+meta+t');
	});
});

describe('KeyRecorder — auto-repeat', () => {
	beforeEach(() => vi.useFakeTimers());
	afterEach(() => vi.useRealTimers());

	it('ignores a held key auto-repeating, so it never chords with itself', () => {
		const onRecord = vi.fn();
		render(<KeyRecorder onRecord={onRecord} />);
		fireEvent.click(screen.getByRole('button'));
		press('e', { ctrlKey: true });
		press('e', { ctrlKey: true, repeat: true });
		vi.advanceTimersByTime(900);
		expect(onRecord).toHaveBeenCalledTimes(1);
		expect(onRecord).toHaveBeenCalledWith('mod+e');
	});
});

describe('KeyRecorder — Backspace and Escape', () => {
	it('Backspace on the first stroke clears instead of recording "backspace"', () => {
		const onRecord = vi.fn();
		render(<KeyRecorder value="mod+e" onRecord={onRecord} />);
		fireEvent.click(screen.getByRole('button'));
		press('Backspace');
		expect(onRecord).toHaveBeenCalledWith('');
	});

	it('Escape cancels without recording', () => {
		const onRecord = vi.fn();
		const onCancel = vi.fn();
		render(<KeyRecorder onRecord={onRecord} onCancel={onCancel} />);
		fireEvent.click(screen.getByRole('button'));
		press('Escape');
		expect(onRecord).not.toHaveBeenCalled();
		expect(onCancel).toHaveBeenCalled();
	});
});
