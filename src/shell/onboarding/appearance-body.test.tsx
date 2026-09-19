// appearance-body — verifies the theme store reads + persists.

import { beforeEach, describe, expect, it } from 'vitest';

import { useIkengaStore } from '@/lib/ikenga/theme-store';

beforeEach(() => {
	// Reset to canonical defaults so previous test state doesn't leak.
	useIkengaStore.setState({
		theme: 'A',
		mode: 'dark',
		density: 'comfortable',
		tintStrength: 'subtle',
		workspace: 'project',
	});
});

describe('appearance step — theme store interactions', () => {
	it('starts with the canonical defaults (A · dark · comfortable)', () => {
		const s = useIkengaStore.getState();
		expect(s.theme).toBe('A');
		expect(s.mode).toBe('dark');
		expect(s.density).toBe('comfortable');
	});

	it('switching theme persists immediately', () => {
		useIkengaStore.getState().setTheme('B');
		expect(useIkengaStore.getState().theme).toBe('B');
		useIkengaStore.getState().setTheme('C');
		expect(useIkengaStore.getState().theme).toBe('C');
	});

	it('switching mode to light persists immediately', () => {
		useIkengaStore.getState().setMode('light');
		expect(useIkengaStore.getState().mode).toBe('light');
	});

	it('switching density persists immediately', () => {
		useIkengaStore.getState().setDensity('compact');
		expect(useIkengaStore.getState().density).toBe('compact');
		useIkengaStore.getState().setDensity('spacious');
		expect(useIkengaStore.getState().density).toBe('spacious');
	});

	it('exposes three controls (theme · mode · density) — sanity', () => {
		// Live-preview rule: each setter exists. The body wires all three.
		const s = useIkengaStore.getState();
		expect(typeof s.setTheme).toBe('function');
		expect(typeof s.setMode).toBe('function');
		expect(typeof s.setDensity).toBe('function');
	});
});
