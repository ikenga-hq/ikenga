// WP-69 — the pure map arithmetic behind multi-surface detached windows.
import { describe, expect, it } from 'vitest';

import {
	applySurfacesChanged,
	PENDING_WINDOW_LABEL,
	returnedByClosedWindows,
	SURFACES_CHANGED_TOPIC,
	surfacesOf,
} from './surfaces-topic';

describe('applySurfacesChanged', () => {
	it('makes the changed window hold exactly its new set, leaving other windows alone', () => {
		const prev = { 'terminal:a': 'detached-w2', 'terminal:b': 'detached-w2', 'viewer:/x.md': 'detached-v' };
		const next = applySurfacesChanged(prev, { label: 'detached-w2', surface_set: ['terminal:b', 'terminal:c'] });
		expect(next).toEqual({ 'terminal:b': 'detached-w2', 'terminal:c': 'detached-w2', 'viewer:/x.md': 'detached-v' });
		expect(prev['terminal:a']).toBe('detached-w2'); // pure
	});

	it('replaces a provisional entry once the join lands', () => {
		const next = applySurfacesChanged(
			{ 'terminal:a': PENDING_WINDOW_LABEL },
			{ label: 'detached-w2', surface_set: ['terminal:a'] }
		);
		expect(next['terminal:a']).toBe('detached-w2');
	});

	it('an empty set drops every surface of that window', () => {
		expect(applySurfacesChanged({ 'terminal:a': 'detached-w2' }, { label: 'detached-w2', surface_set: [] })).toEqual({});
	});
});

describe('surfacesOf', () => {
	it('lists every surface one window holds', () => {
		const map = { 'terminal:a': 'w', 'terminal:b': 'other', 'terminal:c': 'w' };
		expect(surfacesOf(map, 'w')).toEqual(['terminal:a', 'terminal:c']);
		expect(surfacesOf(map, 'none')).toEqual([]);
	});
});

describe('returnedByClosedWindows', () => {
	it('groups surfaces whose window closed, by that window', () => {
		const prev = { 'terminal:a': 'w2', 'terminal:b': 'w2', 'terminal:c': 'w3' };
		expect(returnedByClosedWindows(prev, { 'terminal:c': 'w3' }, new Set(['w3']))).toEqual({
			w2: ['terminal:a', 'terminal:b'],
		});
	});

	it('ignores a surface that moved to another window, one whose window is still open, and provisional entries', () => {
		const prev = { 'terminal:a': 'w2', 'terminal:b': 'w3', 'terminal:p': PENDING_WINDOW_LABEL };
		const next = { 'terminal:a': 'w4' };
		expect(returnedByClosedWindows(prev, next, new Set(['w3', 'w4']))).toEqual({});
	});
});

it('names the host-only topic Rust emits', () => {
	expect(SURFACES_CHANGED_TOPIC).toBe('window://surfaces-changed');
});
