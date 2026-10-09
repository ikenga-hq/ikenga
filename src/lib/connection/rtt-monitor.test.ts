import { describe, expect, it, vi } from 'vitest';
import { RTT_WINDOW } from './rtt';
import {
	type ProbeResult,
	RttMonitor,
	STALE_AFTER_HIDDEN_MS,
	type Visibility,
} from './rtt-monitor';

/** A manual clock + timer queue + visibility switch + scripted probe. */
function harness(results: Array<ProbeResult | 'throw'> = []) {
	let now = 0;
	let hidden = false;
	const visListeners = new Set<() => void>();
	const timers = new Map<number, { at: number; fn: () => void }>();
	let nextId = 1;
	const probe = vi.fn(async (): Promise<ProbeResult> => {
		const next = results.shift();
		if (next === 'throw') throw new Error('boom');
		return next === undefined ? { ms: 100, via: 'ws' } : next;
	});
	const visibility: Visibility = {
		isHidden: () => hidden,
		onChange: (cb) => {
			visListeners.add(cb);
			return () => visListeners.delete(cb);
		},
	};
	const monitor = new RttMonitor({
		probe,
		visibility,
		now: () => now,
		setTimer: (fn, ms) => {
			const id = nextId++;
			timers.set(id, { at: now + ms, fn });
			return id;
		},
		clearTimer: (id) => void timers.delete(id as number),
		intervalMs: 5000,
	});
	return {
		monitor,
		probe,
		timers,
		/** Let pending promise continuations run. */
		flush: async () => {
			for (let i = 0; i < 5; i++) await Promise.resolve();
		},
		/** Run every timer due within `ms`, then flush. */
		advance: async (ms: number) => {
			now += ms;
			for (const [id, t] of [...timers]) {
				if (t.at <= now) {
					timers.delete(id);
					t.fn();
				}
			}
			for (let i = 0; i < 5; i++) await Promise.resolve();
		},
		setHidden: async (h: boolean) => {
			hidden = h;
			for (const cb of [...visListeners]) cb();
			for (let i = 0; i < 5; i++) await Promise.resolve();
		},
	};
}

describe('RttMonitor', () => {
	it('probes once on retain, then every interval, and reports median and jitter', async () => {
		const h = harness([
			{ ms: 300, via: 'ws' },
			{ ms: 380, via: 'ws' },
			{ ms: 340, via: 'ws' },
		]);
		const release = h.monitor.retain();
		await h.flush();
		expect(h.probe).toHaveBeenCalledTimes(1);
		await h.advance(5000);
		await h.advance(5000);
		expect(h.probe).toHaveBeenCalledTimes(3);
		const s = h.monitor.get();
		expect(s.count).toBe(3);
		expect(s.medianMs).toBe(340);
		expect(s.jitterMs).toBeCloseTo((80 + 40) / 2);
		expect(s.level).toBe('bad');
		release();
	});

	it('pauses while the tab is hidden: no probe, no timer', async () => {
		const h = harness();
		h.monitor.retain();
		await h.flush();
		expect(h.probe).toHaveBeenCalledTimes(1);
		await h.setHidden(true);
		expect(h.timers.size).toBe(0);
		await h.advance(60_000);
		expect(h.probe).toHaveBeenCalledTimes(1);
	});

	it('a probe that lands after the tab was hidden records but does not reschedule', async () => {
		let release!: (r: ProbeResult) => void;
		const h = harness();
		h.probe.mockImplementationOnce(() => new Promise<ProbeResult>((r) => (release = r)));
		h.monitor.retain();
		await h.setHidden(true);
		release({ ms: 200, via: 'ws' });
		await h.flush();
		expect(h.monitor.get().count).toBe(1);
		expect(h.timers.size).toBe(0);
	});

	it('probes immediately on becoming visible again, keeping recent samples', async () => {
		const h = harness([
			{ ms: 100, via: 'ws' },
			{ ms: 120, via: 'ws' },
		]);
		h.monitor.retain();
		await h.flush();
		await h.setHidden(true);
		await h.advance(10_000);
		await h.setHidden(false);
		expect(h.probe).toHaveBeenCalledTimes(2);
		expect(h.monitor.get().count).toBe(2);
	});

	it('starts the window over after a long hidden spell', async () => {
		const h = harness([
			{ ms: 100, via: 'ws' },
			{ ms: 500, via: 'ws' },
		]);
		h.monitor.retain();
		await h.flush();
		await h.setHidden(true);
		await h.advance(STALE_AFTER_HIDDEN_MS + 1);
		await h.setHidden(false);
		const s = h.monitor.get();
		expect(s.count).toBe(1);
		expect(s.medianMs).toBe(500);
	});

	it('does not start probing when retained while hidden, and starts on show', async () => {
		const h = harness();
		await h.setHidden(true);
		h.monitor.retain();
		await h.flush();
		expect(h.probe).not.toHaveBeenCalled();
		await h.setHidden(false);
		expect(h.probe).toHaveBeenCalledTimes(1);
	});

	it('counts misses; two in a row is stalled, a hit clears them', async () => {
		const h = harness([{ ms: 50, via: 'ws' }, null, 'throw', { ms: 60, via: 'ws' }]);
		h.monitor.retain();
		await h.flush();
		await h.advance(5000);
		expect(h.monitor.get().failures).toBe(1);
		expect(h.monitor.get().level).toBe('good');
		await h.advance(5000);
		expect(h.monitor.get().failures).toBe(2);
		expect(h.monitor.get().level).toBe('bad');
		await h.advance(5000);
		expect(h.monitor.get().failures).toBe(0);
		expect(h.monitor.get().level).toBe('good');
	});

	it('keeps only the last RTT_WINDOW samples', async () => {
		const h = harness(
			Array.from({ length: RTT_WINDOW + 5 }, (_, i) => ({ ms: 100 + i, via: 'ws' as const }))
		);
		h.monitor.retain();
		await h.flush();
		for (let i = 0; i < RTT_WINDOW + 4; i++) await h.advance(5000);
		expect(h.monitor.get().count).toBe(RTT_WINDOW);
	});

	it('is shared: the last release stops everything and clears the readings', async () => {
		const h = harness();
		const a = h.monitor.retain();
		const b = h.monitor.retain();
		await h.flush();
		expect(h.probe).toHaveBeenCalledTimes(1);
		a();
		expect(h.monitor.active).toBe(true);
		b();
		expect(h.monitor.active).toBe(false);
		expect(h.monitor.get().count).toBe(0);
		await h.advance(60_000);
		expect(h.probe).toHaveBeenCalledTimes(1);
	});

	it('notifies subscribers on every reading', async () => {
		const h = harness();
		const seen: number[] = [];
		h.monitor.subscribe(() => seen.push(h.monitor.get().count));
		h.monitor.retain();
		await h.flush();
		await h.advance(5000);
		expect(seen).toEqual([1, 2]);
	});
});
