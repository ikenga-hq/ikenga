import { describe, expect, it } from 'vitest';

import type { AppLockBiometric } from '@/lib/tauri-cmd';

import {
	ACTIVITY_THROTTLE_MS,
	countGoingRuns,
	lockMetaLine,
	parseIdleMinutes,
	retryLine,
	secretProblem,
	shouldReportActivity,
	stillGoingLine,
	trailingDelay,
	unlockMethodOptions,
} from './app-lock-model';

const NONE: AppLockBiometric = { kind: 'none', label: '', available: false, reason: 'no prompt' };
const HELLO_OFF: AppLockBiometric = {
	kind: 'windows-hello',
	label: 'Windows Hello',
	available: false,
	reason: 'not wired',
};

describe('parseIdleMinutes', () => {
	it('accepts whole minutes in range', () => {
		expect(parseIdleMinutes('15')).toBe(15);
		expect(parseIdleMinutes(' 1 ')).toBe(1);
		expect(parseIdleMinutes('1440')).toBe(1440);
	});

	it('refuses zero, fractions, text and out-of-range values', () => {
		for (const raw of ['0', '1.5', 'ten', '', '-3', '1441']) {
			expect(parseIdleMinutes(raw), raw).toBeNull();
		}
	});
});

describe('secretProblem', () => {
	it('matches the Rust limits', () => {
		expect(secretProblem('123')).toMatch(/at least 4/);
		expect(secretProblem('    ')).toMatch(/spaces/);
		expect(secretProblem('1234')).toBeNull();
		expect(secretProblem('x'.repeat(257))).toMatch(/at most 256/);
	});

	it('counts characters, not UTF-16 units', () => {
		expect(secretProblem('😀😀😀')).not.toBeNull();
		expect(secretProblem('😀😀😀😀')).toBeNull();
	});
});

describe('lockMetaLine', () => {
	it('reads like the design meta line', () => {
		expect(lockMetaLine({ host: 'ned-desktop', reason: 'idle', idleMinutes: 15 })).toBe(
			'ned-desktop · locked after 15 min idle'
		);
		expect(lockMetaLine({ host: 'ned-desktop', reason: 'launch', idleMinutes: 15 })).toContain(
			'at launch'
		);
		expect(lockMetaLine({ host: '', reason: 'manual', idleMinutes: 15 })).toBe(
			'this device · locked with Lock now'
		);
	});
});

describe('unlockMethodOptions', () => {
	it('offers only the PIN where the OS has no biometric', () => {
		expect(unlockMethodOptions(NONE).map((o) => o.id)).toEqual(['pin']);
	});

	it('lists the OS option first, disabled with the reason, while unavailable', () => {
		const opts = unlockMethodOptions(HELLO_OFF);
		expect(opts.map((o) => o.id)).toEqual(['os', 'pin']);
		expect(opts[0]).toMatchObject({ label: 'Windows Hello', disabled: true, why: 'not wired' });
		expect(opts[1].disabled).toBe(false);
	});

	it('enables the OS option once the platform reports it', () => {
		const opts = unlockMethodOptions({ ...HELLO_OFF, available: true });
		expect(opts[0].disabled).toBe(false);
		expect(opts[0].why).toBeUndefined();
	});
});

describe('activity throttle and retry copy', () => {
	it('reports the first activity and then at most once per window', () => {
		expect(shouldReportActivity(null, 0)).toBe(true);
		expect(shouldReportActivity(0, ACTIVITY_THROTTLE_MS - 1)).toBe(false);
		expect(shouldReportActivity(0, ACTIVITY_THROTTLE_MS)).toBe(true);
	});

	it('schedules the trailing send for the end of the throttle window', () => {
		expect(trailingDelay(1_000, 1_000)).toBe(ACTIVITY_THROTTLE_MS);
		expect(trailingDelay(1_000, 21_000)).toBe(ACTIVITY_THROTTLE_MS - 20_000);
		expect(trailingDelay(0, ACTIVITY_THROTTLE_MS + 5)).toBe(0);
	});

	it('rounds the wait up to whole seconds', () => {
		expect(retryLine(29_001)).toBe('Try again in 30 s.');
		expect(retryLine(1)).toBe('Try again in 1 s.');
	});
});

describe('stillGoingLine (D-05 `locked` fine print)', () => {
	it('reads like the design with both counts', () => {
		expect(stillGoingLine(2, 1)).toBe('2 sessions and 1 run are still going');
		expect(stillGoingLine(1, 3)).toBe('1 session and 3 runs are still going');
	});

	it('agrees in number when only one kind is going', () => {
		expect(stillGoingLine(1, 0)).toBe('1 session is still going');
		expect(stillGoingLine(2, 0)).toBe('2 sessions are still going');
		expect(stillGoingLine(0, 1)).toBe('1 run is still going');
		expect(stillGoingLine(0, 0)).toBe('No sessions or runs are going');
	});

	it('keeps the plain claim when the host could not say', () => {
		expect(stillGoingLine(null, 1)).toBe('Sessions and runs keep going');
	});

	it('counts queued, running and awaiting-auth runs, not finished ones', () => {
		const statuses = ['queued', 'running', 'awaiting_auth', 'done', 'failed', 'cancelled', 'timed_out'];
		expect(countGoingRuns(statuses.map((status) => ({ status })))).toBe(3);
	});
});
