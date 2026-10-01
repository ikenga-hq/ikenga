// The T1 sign-in mode of the reauth overlay (G-PRINCIPAL §2.4, OD-12).

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useReauthStore } from '@/lib/transport/reauth-store';
import { __setT1SessionForTests } from '@/lib/transport/t1-session';
import { ReauthOverlay } from './reauth-overlay';

const fetchMock = vi.fn<typeof fetch>();

beforeEach(() => {
	fetchMock.mockReset();
	vi.stubGlobal('fetch', fetchMock);
	useReauthStore.setState({ isOpen: true, errorMsg: null, tokenInput: '' });
});

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
	__setT1SessionForTests(false);
	useReauthStore.setState({ isOpen: false, errorMsg: null });
});

function fill(username: string, password: string) {
	fireEvent.change(screen.getByLabelText('Username'), { target: { value: username } });
	fireEvent.change(screen.getByLabelText('Password'), { target: { value: password } });
	fireEvent.click(screen.getByRole('button', { name: 'Sign in' }));
}

describe('ReauthOverlay under T1', () => {
	it('asks for a username and password, not a token', () => {
		__setT1SessionForTests(true);
		render(<ReauthOverlay />);
		expect(screen.getByRole('form', { name: 'Sign in' })).toBeTruthy();
		expect(screen.getByLabelText('Username')).toBeTruthy();
		expect(screen.getByLabelText('Password').getAttribute('type')).toBe('password');
		expect(screen.queryByPlaceholderText('Paste auth token...')).toBeNull();
	});

	it('signs in at /auth/login with the cookie and shows a failure', async () => {
		__setT1SessionForTests(true);
		fetchMock.mockResolvedValueOnce(
			new Response(JSON.stringify({ ok: false, code: 'invalid_credentials' }), { status: 401 })
		);
		render(<ReauthOverlay />);
		fill('ada', 'wrong');
		await waitFor(() =>
			expect(screen.getByRole('alert').textContent).toBe('Wrong username or password.')
		);
		const [url, init] = fetchMock.mock.calls[0]!;
		expect(url).toBe('/auth/login');
		expect(init).toMatchObject({ method: 'POST', credentials: 'same-origin' });
		expect(JSON.parse(String(init?.body))).toEqual({ username: 'ada', password: 'wrong' });
		// The password field is cleared for the next try; the name stays.
		expect((screen.getByLabelText('Password') as HTMLInputElement).value).toBe('');
		expect((screen.getByLabelText('Username') as HTMLInputElement).value).toBe('ada');
	});

	it('refuses an empty form without a request', async () => {
		__setT1SessionForTests(true);
		render(<ReauthOverlay />);
		fill('', '');
		await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy());
		expect(fetchMock).not.toHaveBeenCalled();
	});

	it('succeeds on 204 and reloads into a normal boot', async () => {
		__setT1SessionForTests(true);
		fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }));
		// jsdom reports the reload as "not implemented" on the console.
		const err = vi.spyOn(console, 'error').mockImplementation(() => {});
		await expect(useReauthStore.getState().signIn('ada', 'correct horse')).resolves.toBe(true);
		expect(String(err.mock.calls[0]?.[0])).toContain('navigation');
		err.mockRestore();
		expect(useReauthStore.getState().errorMsg).toBeNull();
	});

	it('prefills the name of a session that ended mid-use', () => {
		__setT1SessionForTests(true, {
			principal_id: '01890a5d-ac96-774b-bcce-b302099a8057',
			username: 'ada',
			is_admin: false,
		});
		render(<ReauthOverlay />);
		expect(screen.getByText('Sign in again')).toBeTruthy();
		expect((screen.getByLabelText('Username') as HTMLInputElement).value).toBe('ada');
	});
});

describe('ReauthOverlay under T0', () => {
	it('still asks for the token', () => {
		render(<ReauthOverlay />);
		expect(screen.getByPlaceholderText('Paste auth token...')).toBeTruthy();
		expect(screen.queryByLabelText('Username')).toBeNull();
	});
});
