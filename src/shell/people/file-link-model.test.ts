import { describe, expect, it } from 'vitest';

import {
	absoluteFilePath,
	canRevoke,
	createArgs,
	DEFAULT_LINK_DAYS,
	expiryArgs,
	expiryText,
	filterByOwner,
	isLinksAdmin,
	LINK_EXPIRY_CHOICES,
	linkBlocker,
	linksOffered,
	ownersOf,
	passcodeProblem,
} from './file-link-model';

describe('expiry', () => {
	it('offers 1 / 7 / 30 days and never, defaulting to 7', () => {
		expect(LINK_EXPIRY_CHOICES.map((c) => c.days)).toEqual([1, 7, 30, null]);
		expect(DEFAULT_LINK_DAYS).toBe(7);
	});
	it('sends days, or never', () => {
		expect(expiryArgs(30)).toEqual({ expiresInDays: 30 });
		expect(expiryArgs(null)).toEqual({ never: true });
	});
	it('words the time left', () => {
		const now = 1_000_000;
		expect(expiryText(null, now)).toBe('Never');
		expect(expiryText(now - 1, now)).toBe('Ended');
		expect(expiryText(now + 5 * 3_600_000, now)).toBe('In 5 h');
		expect(expiryText(now + 6 * 86_400_000, now)).toBe('In 6 d');
	});
});

describe('passcode', () => {
	it('is optional and 6..128 characters', () => {
		expect(passcodeProblem('')).toBeNull();
		expect(passcodeProblem('12345')).toMatch(/at least 6/);
		expect(passcodeProblem('123456')).toBeNull();
		expect(passcodeProblem('x'.repeat(128))).toBeNull();
		expect(passcodeProblem('x'.repeat(129))).toMatch(/at most 128/);
	});
	it('counts characters, not UTF-16 units', () => {
		expect(passcodeProblem('😀'.repeat(6))).toBeNull();
	});
});

describe('create args and blocker', () => {
	it('omits an empty passcode', () => {
		expect(createArgs({ path: '/h/a.pdf', days: 7, passcode: '' })).toEqual({
			path: '/h/a.pdf',
			expiresInDays: 7,
		});
		expect(createArgs({ path: '/h/a.pdf', days: null, passcode: 'secret1' })).toEqual({
			path: '/h/a.pdf',
			never: true,
			passcode: 'secret1',
		});
	});
	it('blocks no file, a bad expiry and a short passcode', () => {
		expect(linkBlocker({ path: ' ', days: 7, passcode: '' })).toMatch(/file/);
		expect(linkBlocker({ path: '/a', days: 0, passcode: '' })).toMatch(/expires/);
		expect(linkBlocker({ path: '/a', days: 400, passcode: '' })).toMatch(/expires/);
		expect(linkBlocker({ path: '/a', days: 7, passcode: 'abc' })).toMatch(/at least/);
		expect(linkBlocker({ path: '/a', days: null, passcode: '' })).toBeNull();
	});
});

describe('visibility', () => {
	it('offers links only when the server enabled them and outside a share', () => {
		expect(linksOffered(undefined)).toBe(false);
		expect(linksOffered({ fileLinksEnabled: false, share: null })).toBe(false);
		expect(linksOffered({ fileLinksEnabled: true, share: null })).toBe(true);
		expect(
			linksOffered({
				fileLinksEnabled: true,
				share: { projectKey: 'k', projectName: 'n', ownerUsername: 'o', role: 'guest', scope: 'artifact' },
			})
		).toBe(false);
	});
	it('shows the admin view to an admin at admin strength only', () => {
		const p = (isAdmin: boolean) => ({ principalId: 'p', username: 'u', isAdmin });
		expect(isLinksAdmin({ principal: p(true), adminStrength: true })).toBe(true);
		expect(isLinksAdmin({ principal: p(true), adminStrength: false })).toBe(false);
		expect(isLinksAdmin({ principal: p(false), adminStrength: true })).toBe(false);
		expect(isLinksAdmin(null)).toBe(false);
	});
});

describe('list helpers', () => {
	it('revokes only live or locked links', () => {
		expect(canRevoke({ state: 'live', revokedAt: null })).toBe(true);
		expect(canRevoke({ state: 'locked', revokedAt: null })).toBe(true);
		expect(canRevoke({ state: 'expired', revokedAt: null })).toBe(false);
		expect(canRevoke({ state: 'revoked', revokedAt: 5 })).toBe(false);
	});
	it('filters and lists owners', () => {
		const rows = [
			{ ownerPrincipalId: 'a', ownerUsername: 'ada' },
			{ ownerPrincipalId: 'b', ownerUsername: null },
			{ ownerPrincipalId: 'a', ownerUsername: 'ada' },
		];
		expect(filterByOwner(rows, null)).toHaveLength(3);
		expect(filterByOwner(rows, 'a')).toHaveLength(2);
		expect(ownersOf(rows)).toEqual([
			{ id: 'a', name: 'ada' },
			{ id: 'b', name: 'b' },
		]);
	});
	it('joins a root and a relative path', () => {
		expect(absoluteFilePath('/h/p/', 'a/b.pdf')).toBe('/h/p/a/b.pdf');
		expect(absoluteFilePath(null, 'a')).toBeNull();
	});
});
