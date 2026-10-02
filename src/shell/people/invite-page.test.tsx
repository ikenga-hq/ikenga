// WP-76: `/remote/invite` (G-ACCESS §7.3): the token rides the fragment;
// inspect, then accept as a new account (only when allowed) or as the
// signed-in one; success opens the share.

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { __resetShareModeForTests, currentShare } from '@/lib/transport';
import { InvitePage, inviteErrorCopy, tokenFromHash } from '@/routes/remote/invite';

const fetchMock = vi.fn<typeof fetch>();
const TOKEN = 'iki1.01890a5d-ac96-774b-bcce-b302099a8057.c2VjcmV0';
const INFO = {
	ok: true,
	project_name: 'royalti-co',
	owner_username: 'ned',
	role: 'reviewer',
	scope: 'project',
	expires_at: Date.now() + 86_400_000,
	allow_new_account: true,
};

beforeEach(() => {
	fetchMock.mockReset();
	vi.stubGlobal('fetch', fetchMock);
	sessionStorage.clear();
	__resetShareModeForTests();
	window.history.replaceState(null, '', `/remote/invite#t=${TOKEN}`);
});

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

function json(body: unknown, status = 200) {
	return new Response(JSON.stringify(body), { status });
}

describe('invite helpers', () => {
	it('reads only an iki1 token from the fragment', () => {
		expect(tokenFromHash(`#t=${TOKEN}`)).toBe(TOKEN);
		expect(tokenFromHash('#t=ikd1.x.y')).toBeNull();
		expect(tokenFromHash('')).toBeNull();
		expect(inviteErrorCopy(410, null)).toMatch(/used, revoked or has expired/);
		expect(inviteErrorCopy(429, null)).toMatch(/Too many/);
	});
});

describe('InvitePage', () => {
	it('inspects, then creates an account and opens the share', async () => {
		fetchMock.mockImplementation(async (url) => {
			if (url === '/access/invite/inspect') return json(INFO);
			if (url === '/auth/me') return json({}, 401);
			if (url === '/access/invite/accept')
				return json({
					ok: true,
					share: {
						projectKey: '01890a5d-ac96-774b-bcce-b302099a8057/royalti-co',
						ownerPrincipalId: '01890a5d-ac96-774b-bcce-b302099a8057',
						projectId: 'royalti-co',
						projectName: 'royalti-co',
						role: 'reviewer',
					},
				});
			return json({}, 404);
		});
		const err = vi.spyOn(console, 'error').mockImplementation(() => {});
		render(<InvitePage />);
		expect(await screen.findByRole('form', { name: 'Accept invite' })).toBeTruthy();
		expect(window.location.hash).toBe('');
		fireEvent.change(screen.getByLabelText('Username'), { target: { value: 'tomi' } });
		fireEvent.change(screen.getByLabelText(/Password/), {
			target: { value: 'correct horse battery' },
		});
		fireEvent.click(screen.getByRole('button', { name: 'Create account and join' }));
		await waitFor(() => expect(currentShare()?.projectId).toBe('royalti-co'));
		const accept = fetchMock.mock.calls.find(([u]) => u === '/access/invite/accept')!;
		expect(JSON.parse(String(accept[1]?.body))).toEqual({
			token: TOKEN,
			username: 'tomi',
			password: 'correct horse battery',
		});
		err.mockRestore();
	});

	it('an invite for existing accounts offers only sign-in', async () => {
		fetchMock.mockImplementation(async (url) => {
			if (url === '/access/invite/inspect') return json({ ...INFO, allow_new_account: false });
			return json({}, 401);
		});
		render(<InvitePage />);
		expect(await screen.findByRole('button', { name: 'Sign in to accept' })).toBeTruthy();
		expect(screen.queryByRole('button', { name: 'Create an account' })).toBeNull();
		expect(screen.getByText(/Ask an admin/)).toBeTruthy();
	});

	it('a dead link says so', async () => {
		fetchMock.mockImplementation(async (url) =>
			url === '/access/invite/inspect' ? json({ ok: false, error: 'gone' }, 410) : json({}, 401)
		);
		render(<InvitePage />);
		expect(await screen.findByRole('alert')).toBeTruthy();
		expect(screen.getByRole('alert').textContent).toMatch(/not valid/);
	});
});
