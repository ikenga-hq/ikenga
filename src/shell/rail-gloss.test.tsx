// First-contact gloss (R3): shows once per term, never sticks — dismissed by
// pointer, key or 4 s — and remembered in localStorage 'ikenga.gloss.seen'.

import { act, cleanup, render } from '@testing-library/react';
import { useRef } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	GLOSS_ARM_MS,
	GLOSS_SEEN_KEY,
	GLOSS_TIMEOUT_MS,
	pickGlossTerm,
	RailGloss,
	type RailGlossTerm,
} from './rail-gloss';

const TERMS: RailGlossTerm[] = [
	{ term: 'ngwa', text: 'Ngwa — your equipment', keyLabel: 'Ctrl+3', anchor: 'ngwa' },
	{ term: 'chi', text: 'Chi — your engine session', keyLabel: 'Ctrl+2', anchor: 'chi' },
];

function Rail() {
	const ref = useRef<HTMLElement>(null);
	return (
		<nav ref={ref} style={{ position: 'relative' }}>
			<button type="button" data-rail-item="chi">
				Chi
			</button>
			<button type="button" data-rail-item="ngwa">
				Ngwa
			</button>
			<RailGloss terms={TERMS} railRef={ref} />
		</nav>
	);
}

const gloss = () => document.querySelector<HTMLElement>('[role="tooltip"]#rail-gloss');
const seen = () => JSON.parse(localStorage.getItem(GLOSS_SEEN_KEY) ?? 'null');

beforeEach(() => {
	localStorage.clear();
	vi.useFakeTimers();
});

afterEach(() => {
	cleanup();
	vi.useRealTimers();
	localStorage.clear();
});

describe('RailGloss', () => {
	it('shows the first unseen term, pointing at its key, then expires after 4 s', () => {
		render(<Rail />);
		expect(gloss()?.dataset.glossTerm).toBe('ngwa');
		expect(gloss()?.textContent).toBe('Ngwa — your equipmentCtrl+3');
		expect(gloss()?.style.visibility).not.toBe('hidden');
		const ngwaKey = document.querySelector('[data-rail-item="ngwa"]')!;
		expect(ngwaKey.getAttribute('aria-describedby')).toBe('rail-gloss');

		act(() => {
			vi.advanceTimersByTime(GLOSS_TIMEOUT_MS - 1);
		});
		expect(gloss()).not.toBeNull();
		act(() => {
			vi.advanceTimersByTime(1);
		});
		expect(gloss()).toBeNull();
		expect(ngwaKey.hasAttribute('aria-describedby')).toBe(false);
		expect(seen()).toEqual(['ngwa']);
	});

	it('is dismissed by a pointer event once armed, and marked seen', () => {
		render(<Rail />);
		// Inside the grace window a pointer event does not dismiss it.
		act(() => {
			document.body.dispatchEvent(new Event('pointerdown', { bubbles: true }));
		});
		expect(gloss()).not.toBeNull();
		act(() => {
			vi.advanceTimersByTime(GLOSS_ARM_MS);
		});
		act(() => {
			document.body.dispatchEvent(new Event('pointerdown', { bubbles: true }));
		});
		expect(gloss()).toBeNull();
		expect(seen()).toEqual(['ngwa']);
	});

	it('is dismissed by a key press once armed', () => {
		render(<Rail />);
		act(() => {
			vi.advanceTimersByTime(GLOSS_ARM_MS);
		});
		act(() => {
			document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'a', bubbles: true }));
		});
		expect(gloss()).toBeNull();
		expect(seen()).toEqual(['ngwa']);
	});

	it('shows once: a later launch glosses the next term, then nothing', () => {
		const first = render(<Rail />);
		act(() => {
			vi.advanceTimersByTime(GLOSS_TIMEOUT_MS);
		});
		first.unmount();

		const second = render(<Rail />);
		expect(gloss()?.dataset.glossTerm).toBe('chi');
		act(() => {
			vi.advanceTimersByTime(GLOSS_TIMEOUT_MS);
		});
		expect(gloss()).toBeNull();
		expect(seen()).toEqual(['ngwa', 'chi']);
		second.unmount();

		render(<Rail />);
		expect(gloss()).toBeNull();
		act(() => {
			vi.advanceTimersByTime(GLOSS_TIMEOUT_MS * 2);
		});
		expect(gloss()).toBeNull();
	});

	it('never sticks: timers and listeners are gone after dismissal', () => {
		const add = vi.spyOn(document, 'addEventListener');
		const remove = vi.spyOn(document, 'removeEventListener');
		render(<Rail />);
		act(() => {
			vi.advanceTimersByTime(GLOSS_TIMEOUT_MS);
		});
		expect(gloss()).toBeNull();
		const TYPES = ['pointerdown', 'pointermove', 'keydown'];
		const dismissers = (calls: unknown[][]) =>
			calls.filter(([t, , capture]) => TYPES.includes(String(t)) && capture === true);
		const added = dismissers(add.mock.calls);
		expect(added.map(([t]) => t)).toEqual(TYPES);
		// Every armed dismiss listener was removed again, with the same handler.
		const removed = dismissers(remove.mock.calls);
		for (const [type, handler] of added) {
			expect(removed.some(([t, h]) => t === type && h === handler)).toBe(true);
		}
		// Nothing brings it back.
		act(() => {
			vi.advanceTimersByTime(GLOSS_TIMEOUT_MS * 3);
			document.body.dispatchEvent(new Event('pointerdown', { bubbles: true }));
		});
		expect(gloss()).toBeNull();
	});

	it('does not show when storage cannot be read, and tolerates garbage', () => {
		const get = vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
			throw new Error('blocked');
		});
		expect(pickGlossTerm(TERMS)).toBeNull();
		render(<Rail />);
		expect(gloss()).toBeNull();
		get.mockRestore();

		localStorage.setItem(GLOSS_SEEN_KEY, '{not json');
		expect(pickGlossTerm(TERMS)?.term).toBe('ngwa');
		localStorage.setItem(GLOSS_SEEN_KEY, JSON.stringify({ ngwa: true }));
		expect(pickGlossTerm(TERMS)?.term).toBe('ngwa');
	});
});
