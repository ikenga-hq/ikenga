import { describe, expect, it } from 'vitest';
import {
	classifyRtt,
	EMPTY_RTT,
	explainRtt,
	formatRtt,
	jitter,
	median,
	RTT_BAD_MS,
	RTT_GOOD_MS,
	summarizeRtt,
} from './rtt';

describe('median', () => {
	it('is null for no samples', () => expect(median([])).toBeNull());
	it('takes the middle of an odd count regardless of order', () => {
		expect(median([300, 100, 200])).toBe(200);
	});
	it('averages the middle pair of an even count', () => {
		expect(median([100, 400, 200, 300])).toBe(250);
	});
	it('is not moved by one outlier', () => {
		expect(median([310, 330, 5000, 340, 320])).toBe(330);
	});
	it('does not reorder its input', () => {
		const xs = [3, 1, 2];
		median(xs);
		expect(xs).toEqual([3, 1, 2]);
	});
});

describe('jitter', () => {
	it('needs three samples', () => {
		expect(jitter([])).toBeNull();
		expect(jitter([100, 200])).toBeNull();
	});
	it('is the mean absolute difference between consecutive samples, in order', () => {
		// |200-100| + |150-200| + |250-150| = 250 over 3 gaps
		expect(jitter([100, 200, 150, 250])).toBeCloseTo(250 / 3);
	});
	it('is zero for a perfectly steady link', () => {
		expect(jitter([300, 300, 300, 300])).toBe(0);
	});
	it('depends on order, not just spread', () => {
		expect(jitter([100, 100, 300, 300])).toBeLessThan(jitter([100, 300, 100, 300]) as number);
	});
});

describe('classifyRtt thresholds', () => {
	it('is good up to and including 150 ms', () => {
		expect(classifyRtt(0)).toBe('good');
		expect(classifyRtt(RTT_GOOD_MS)).toBe('good');
	});
	it('is amber above 150 and up to 300', () => {
		expect(classifyRtt(RTT_GOOD_MS + 1)).toBe('warn');
		expect(classifyRtt(RTT_BAD_MS)).toBe('warn');
	});
	it('is red above 300', () => {
		expect(classifyRtt(RTT_BAD_MS + 1)).toBe('bad');
		expect(classifyRtt(490)).toBe('bad');
	});
	it('is unknown without a number', () => {
		expect(classifyRtt(null)).toBe('unknown');
		expect(classifyRtt(Number.NaN)).toBe('unknown');
	});
});

describe('summarizeRtt / formatRtt', () => {
	it('formats the Lagos case as "340 ms ± 85"', () => {
		// median 340, consecutive diffs average 85
		const samples = [340, 255, 340, 425, 340, 255, 340, 425, 340];
		const s = summarizeRtt(samples, 0, 'ws');
		expect(s.medianMs).toBe(340);
		expect(Math.round(s.jitterMs as number)).toBe(85);
		expect(s.level).toBe('bad');
		expect(formatRtt(s)).toBe('340 ms ± 85');
	});
	it('leaves jitter off until it can be computed', () => {
		expect(formatRtt(summarizeRtt([120, 130], 0, 'ws'))).toBe('125 ms');
	});
	it('says Measuring before any sample', () => {
		expect(formatRtt(EMPTY_RTT)).toBe('Measuring…');
	});
	it('two misses in a row is red "No response" whatever the old samples say', () => {
		const s = summarizeRtt([40, 45, 50], 2, 'ws');
		expect(s.level).toBe('bad');
		expect(formatRtt(s)).toBe('No response');
		expect(explainRtt(s)).toMatch(/not answered/);
	});
	it('one miss does not turn it red', () => {
		expect(summarizeRtt([40, 45, 50], 1, 'ws').level).toBe('good');
	});
	it('explains typing lag in the red state', () => {
		expect(explainRtt(summarizeRtt([400, 410, 420], 0, 'ws'))).toMatch(/Typing will lag/);
	});
});
