import { act, renderHook } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/transport', async (importOriginal) => {
	const actual = await importOriginal<typeof import('@/lib/transport')>();
	return { ...actual, isBrowserSession: () => true };
});

import { type ProbeResult, RttMonitor, __setRttMonitorForTests, useConnectionRtt } from './rtt-monitor';

/** A monitor whose probe answers at once, so every probe notifies subscribers. */
function liveMonitor() {
	const probe = vi.fn(async (): Promise<ProbeResult> => ({ ms: 120, via: 'ws' }));
	const monitor = new RttMonitor({
		probe,
		visibility: { isHidden: () => false, onChange: () => () => {} },
		now: () => 0,
		setTimer: () => 0,
		clearTimer: () => {},
		intervalMs: 5000,
	});
	return { monitor, probe };
}

afterEach(() => __setRttMonitorForTests(null));

describe('useConnectionRtt', () => {
	it('keeps one subscription across re-renders: no restart, no probe storm, samples accumulate', async () => {
		const { monitor, probe } = liveMonitor();
		const retain = vi.spyOn(monitor, 'retain');
		__setRttMonitorForTests(monitor);

		const { result, rerender } = renderHook(() => useConnectionRtt());
		// Let the first probe resolve; it notifies, which re-renders the hook.
		await act(async () => {
			for (let i = 0; i < 5; i++) await Promise.resolve();
		});
		for (let i = 0; i < 10; i++) rerender();
		await act(async () => {
			for (let i = 0; i < 5; i++) await Promise.resolve();
		});

		// An unstable `subscribe` released + retained on every render, which
		// stopped the monitor (clearing samples) and probed again each time.
		expect(retain).toHaveBeenCalledTimes(1);
		expect(probe).toHaveBeenCalledTimes(1);
		expect(result.current.count).toBeGreaterThan(0);
	}, 5_000); // the unfixed hook re-renders forever; fail fast instead of hanging
});
