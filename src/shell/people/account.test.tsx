// WP-76: D-05 `profile-account` (G-ACCESS §2.2, G-98) and "Shared with you"
// / share mode (§4.5.2).

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { __resetShareModeForTests, currentShare, withShareQuery } from '@/lib/transport';
import { __setT1SessionForTests } from '@/lib/transport/t1-session';

import { AccountBlock, CHANGE_PASSWORD_COPY, changePassword, SIGN_OUT_COPY } from './account';
import { bannerLine, ShareModeBanner, type ShareView, selectionOf } from './shared-with-you';

const fetchMock = vi.fn<typeof fetch>();

beforeEach(() => {
	fetchMock.mockReset();
	vi.stubGlobal('fetch', fetchMock);
	sessionStorage.clear();
	__resetShareModeForTests();
});

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
	__setT1SessionForTests(false);
});

describe('AccountBlock', () => {
	it('shows the username, the principal id and the admin chip; no sync list (D-2)', async () => {
		__setT1SessionForTests(true, {
			principal_id: '01890a5d-ac96-774b-bcce-b302099a8057',
			username: 'ned',
			is_admin: true,
		});
		fetchMock.mockResolvedValue(new Response('{}', { status: 401 }));
		render(<AccountBlock />);
		expect(screen.getByText('ned')).toBeTruthy();
		expect(screen.getByText('01890a5d-ac96-774b-bcce-b302099a8057')).toBeTruthy();
		expect(screen.getByText('admin')).toBeTruthy();
		expect(screen.queryByText(/Syncs/)).toBeNull();
		expect(screen.getByRole('button', { name: /Change password/ })).toBeTruthy();
		expect(screen.getByRole('button', { name: /Sign out/ })).toBeTruthy();
	});

	it('the confirm copy matches §2.2 / §3.10', () => {
		expect(SIGN_OUT_COPY).toContain('Your paired devices stay paired');
		expect(CHANGE_PASSWORD_COPY).toContain('Every other browser signs out');
	});

	it('maps /auth/password answers', async () => {
		fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }));
		await expect(changePassword('a', 'b'.repeat(12))).resolves.toEqual({ ok: true });
		const [url, init] = fetchMock.mock.calls[0]!;
		expect(url).toBe('/auth/password');
		expect(JSON.parse(String(init?.body))).toEqual({ current: 'a', new: 'b'.repeat(12) });
		fetchMock.mockResolvedValueOnce(new Response('{}', { status: 401 }));
		await expect(changePassword('a', 'b')).resolves.toMatchObject({ ok: false, reason: 'wrong' });
		fetchMock.mockResolvedValueOnce(new Response('{}', { status: 400 }));
		await expect(changePassword('a', 'b')).resolves.toMatchObject({ ok: false, reason: 'policy' });
	});
});

describe('share mode', () => {
	const view: ShareView = {
		projectKey: '01890a5d-ac96-774b-bcce-b302099a8057/royalti-co',
		ownerPrincipalId: '01890a5d-ac96-774b-bcce-b302099a8057',
		ownerUsername: 'ada',
		projectId: 'royalti-co',
		projectName: 'royalti-co',
		role: 'reviewer',
		scope: 'project',
		artifactPath: null,
		expiresAt: null,
		addedAt: 1,
	};

	it('the banner reads "royalti-co · shared by ada · Reviewer"', () => {
		expect(bannerLine(selectionOf(view))).toBe('royalti-co · shared by ada · Reviewer');
	});

	it('selecting a share tags every WebSocket URL and shows the banner', async () => {
		const { setShareMode } = await import('@/lib/transport');
		expect(withShareQuery('ws://h/ws/fs')).toBe('ws://h/ws/fs');
		setShareMode(selectionOf(view));
		expect(currentShare()?.projectKey).toBe(view.projectKey);
		expect(withShareQuery('ws://h/ws/chat/x?token=t')).toBe(
			`ws://h/ws/chat/x?token=t&share=${encodeURIComponent(view.projectKey)}`
		);
		render(<ShareModeBanner />);
		expect(screen.getByText('royalti-co · shared by ada · Reviewer')).toBeTruthy();
		expect(screen.getByRole('button', { name: /Leave shared project/ })).toBeTruthy();
		setShareMode(null);
		expect(withShareQuery('ws://h/ws/fs')).toBe('ws://h/ws/fs');
	});

	it('refuses a malformed selection', async () => {
		const { setShareMode } = await import('@/lib/transport');
		setShareMode({ ...selectionOf(view), projectKey: 'not-a-key' });
		expect(currentShare()).toBeNull();
	});
});
