import { describe, expect, it } from 'vitest';

import { formatRelativeTime } from './relative-time';

const NOW = new Date('2026-05-18T22:30:00Z').getTime();
const min = 60_000;

describe('formatRelativeTime', () => {
	it('renders just-now for <1 min', () => {
		expect(formatRelativeTime(NOW - 30_000, NOW)).toBe('just now');
		expect(formatRelativeTime(NOW, NOW)).toBe('just now');
	});

	it('renders singular for 1 min', () => {
		expect(formatRelativeTime(NOW - 1 * min, NOW)).toBe('a min ago');
	});

	it('renders mins-ago up to 59', () => {
		expect(formatRelativeTime(NOW - 8 * min, NOW)).toBe('8 mins ago');
		expect(formatRelativeTime(NOW - 59 * min, NOW)).toBe('59 mins ago');
	});

	it('renders an-hour-ago for 60-119', () => {
		expect(formatRelativeTime(NOW - 65 * min, NOW)).toBe('an hour ago');
		expect(formatRelativeTime(NOW - 119 * min, NOW)).toBe('an hour ago');
	});

	it('renders 12-hour clock for <24hr', () => {
		// 5 hours ago — only assert the format, not the exact value (tz-dep).
		const s = formatRelativeTime(NOW - 5 * 60 * min, NOW);
		expect(s).toMatch(/^\d{1,2}:\d{2} (AM|PM)$/);
	});

	it('renders yesterday for 24-47 hr', () => {
		expect(formatRelativeTime(NOW - 30 * 60 * min, NOW)).toBe('yesterday');
	});

	it('renders weekday abbrev for 2-6 days', () => {
		const s = formatRelativeTime(NOW - 3 * 24 * 60 * min, NOW);
		// Locale-dependent — assert short shape (3 letters, e.g. "Fri").
		expect(s.length).toBeLessThanOrEqual(4);
	});

	it('renders last week for 7-13 days', () => {
		expect(formatRelativeTime(NOW - 10 * 24 * 60 * min, NOW)).toBe('last week');
	});

	it('renders month + day for >=14 days', () => {
		const s = formatRelativeTime(NOW - 30 * 24 * 60 * min, NOW);
		expect(s).toMatch(/([A-Z][a-z]{2,} \d{1,2}|\d{1,2} [A-Z][a-z]{2,})/);
	});

	it('treats future timestamps as just-now', () => {
		expect(formatRelativeTime(NOW + 1000, NOW)).toBe('just now');
	});
});
