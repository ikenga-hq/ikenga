import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const getAppVersion = vi.fn();

vi.mock('@/lib/transport', () => ({
	getAppVersion: () => getAppVersion(),
}));

import { consumePendingRestartIfMatching, markPendingRestart } from './post-restart';

beforeEach(() => {
	localStorage.clear();
	getAppVersion.mockReset();
});
afterEach(() => {
	localStorage.clear();
});

describe('post-restart marker', () => {
	it('round-trips when the running version matches what was marked', async () => {
		markPendingRestart({ version: '0.9.1', notes: '14 commits', sessionsBefore: 2 });
		getAppVersion.mockResolvedValue('0.9.1');

		const result = await consumePendingRestartIfMatching();
		expect(result).toMatchObject({ version: '0.9.1', notes: '14 commits', sessionsBefore: 2 });
	});

	it('is consumed at most once — a second read finds nothing', async () => {
		markPendingRestart({ version: '0.9.1', notes: '', sessionsBefore: 0 });
		getAppVersion.mockResolvedValue('0.9.1');

		await consumePendingRestartIfMatching();
		const second = await consumePendingRestartIfMatching();
		expect(second).toBeNull();
	});

	it('drops the marker without surfacing it when the running version does not match', async () => {
		// The relaunch landed on a different build than the one we marked —
		// e.g. a second, newer update installed before the first restart ran.
		markPendingRestart({ version: '0.9.1', notes: '', sessionsBefore: 0 });
		getAppVersion.mockResolvedValue('0.9.2');

		const result = await consumePendingRestartIfMatching();
		expect(result).toBeNull();
		// And it's gone — not left around to be misread as matching 0.9.2 later.
		getAppVersion.mockResolvedValue('0.9.2');
		expect(await consumePendingRestartIfMatching()).toBeNull();
	});

	it('returns null when nothing was marked', async () => {
		getAppVersion.mockResolvedValue('0.9.1');
		expect(await consumePendingRestartIfMatching()).toBeNull();
		expect(getAppVersion).not.toHaveBeenCalled();
	});

	it('drops a corrupt marker instead of throwing', async () => {
		localStorage.setItem('ikenga.updater.pending-restart', '{not json');
		getAppVersion.mockResolvedValue('0.9.1');
		expect(await consumePendingRestartIfMatching()).toBeNull();
	});
});
