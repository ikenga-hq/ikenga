// WP-69 — which tab is active in a multi-surface detached window (D-09
// `popOut` activates the joined tab; `moveBack` clamps to the same index).
import { describe, expect, it } from 'vitest';

import { fallbackTabLabel } from './tab-label';
import { nextActiveSurface } from './use-window-surfaces';

describe('nextActiveSurface', () => {
	it('a join activates the surface it added', () => {
		expect(nextActiveSurface(['a'], 'a', ['a', 'b'], ['b'])).toBe('b');
	});

	it('the active tab stays when it is still there', () => {
		expect(nextActiveSurface(['a', 'b', 'c'], 'b', ['b', 'c'])).toBe('b');
	});

	it('when the active tab leaves, the tab now at its index takes over (clamped)', () => {
		expect(nextActiveSurface(['a', 'b', 'c'], 'b', ['a', 'c'])).toBe('c');
		expect(nextActiveSurface(['a', 'b'], 'b', ['a'])).toBe('a');
	});

	it('an empty window has no active tab', () => {
		expect(nextActiveSurface(['a'], 'a', [])).toBeNull();
	});
});

describe('fallbackTabLabel', () => {
	it('names a terminal by its PTY id and a viewer by its file', () => {
		expect(fallbackTabLabel('terminal:0123456789ab')).toBe('terminal 01234567');
		expect(fallbackTabLabel('viewer:/home/u/notes/a,b.md')).toBe('a,b.md');
	});
});
