import { describe, expect, it } from 'vitest';
import { cronToWords, cronToWordsDialect } from './cron-words';

describe('cronToWords', () => {
	it('describes a daily time', () => {
		expect(cronToWords('0 5 * * *')).toBe('Every day at 05:00');
		expect(cronToWords('15 5 * * *')).toBe('Every day at 05:15');
	});

	it('describes a weekly time by weekday number', () => {
		expect(cronToWords('0 9 * * 1')).toBe('Every Monday at 09:00');
		expect(cronToWords('0 9 * * 0')).toBe('Every Sunday at 09:00');
	});

	it('describes a minute step', () => {
		expect(cronToWords('*/15 * * * *')).toBe('Every 15 minutes');
		expect(cronToWords('*/1 * * * *')).toBe('Every 1 minute');
	});

	it('describes an hour step at minute 0', () => {
		expect(cronToWords('0 */4 * * *')).toBe('Every 4 hours');
	});

	it('describes every minute', () => {
		expect(cronToWords('* * * * *')).toBe('Every minute');
	});

	it('falls back to the raw expression for day-of-month/month schedules', () => {
		expect(cronToWords('0 9 1 * *')).toBe('at 0 9 1 * *');
		expect(cronToWords('0 9 * 12 *')).toBe('at 0 9 * 12 *');
	});

	it('falls back to the raw expression for a malformed field count', () => {
		expect(cronToWords('not a cron')).toBe('at not a cron');
	});

	it('falls back to the raw expression for an unrecognized weekday list', () => {
		expect(cronToWords('0 9 * * mon-fri')).toBe('at 0 9 * * mon-fri');
	});

	it('describes a weekday range', () => {
		expect(cronToWords('0 9 * * 1-5')).toBe('Every Monday-Friday at 09:00');
	});

	it('falls back for a reversed or degenerate weekday range', () => {
		expect(cronToWords('0 9 * * 5-1')).toBe('at 0 9 * * 5-1');
		expect(cronToWords('0 9 * * 1-1')).toBe('at 0 9 * * 1-1');
	});
});

describe('cronToWordsDialect', () => {
	it('describes a 6-field agent-ops expression with a zero seconds field', () => {
		expect(cronToWordsDialect('0 0 5 * * *', '6f')).toBe('Every day at 05:00');
	});

	it('falls back for a non-zero seconds field', () => {
		expect(cronToWordsDialect('30 0 5 * * *', '6f')).toBe('at 30 0 5 * * *');
	});

	it('treats a 5f dialect the same as cronToWords', () => {
		expect(cronToWordsDialect('0 5 * * *', '5f')).toBe(cronToWords('0 5 * * *'));
	});
});
