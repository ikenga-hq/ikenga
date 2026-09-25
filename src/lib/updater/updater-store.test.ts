import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('./updater', () => ({
	checkForUpdate: vi.fn(),
	installUpdate: vi.fn(),
	restartApp: vi.fn(),
}));

import { _resetUpdaterPollingForTests, ensureUpdaterPolling, progressPct } from './updater-store';

describe('progressPct', () => {
	it('returns null when the total is unknown (never invents a number)', () => {
		expect(progressPct(500, null)).toBeNull();
		expect(progressPct(500, 0)).toBeNull();
	});

	it('rounds to the nearest percent and clamps at 100', () => {
		expect(progressPct(1, 3)).toBe(33);
		expect(progressPct(1200, 1000)).toBe(100);
	});
});

describe('ensureUpdaterPolling', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		_resetUpdaterPollingForTests();
	});
	afterEach(() => {
		_resetUpdaterPollingForTests();
		vi.useRealTimers();
	});

	it('starts at most one interval regardless of how many times it is called', () => {
		const spy = vi.spyOn(window, 'setInterval');
		ensureUpdaterPolling();
		ensureUpdaterPolling();
		ensureUpdaterPolling();
		expect(spy).toHaveBeenCalledTimes(1);
	});
});
