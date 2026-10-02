// Under T1 the displayed name is the principal's `/auth/me` username, not
// the child's Unix user (G-PRINCIPAL §5 row 7).

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { osUsername } from '@/lib/tauri-cmd';
import { __setT1SessionForTests } from './t1-session';

const fetchMock = vi.fn<typeof fetch>();

beforeEach(() => {
	fetchMock.mockReset();
	vi.stubGlobal('fetch', fetchMock);
});

afterEach(() => {
	vi.unstubAllGlobals();
	__setT1SessionForTests(false);
});

describe('osUsername under T1', () => {
	it('is the signed-in username, without an os_username RPC', async () => {
		__setT1SessionForTests(true, { principal_id: 'p', username: 'ada', is_admin: false });
		expect(await osUsername()).toBe('ada');
		expect(fetchMock).not.toHaveBeenCalled();
	});

	it('asks /auth/me when the principal is not known yet', async () => {
		__setT1SessionForTests(true);
		fetchMock.mockResolvedValueOnce(
			new Response(JSON.stringify({ principal_id: 'p', username: 'bo', is_admin: true }), {
				status: 200,
			})
		);
		expect(await osUsername()).toBe('bo');
		expect(fetchMock).toHaveBeenCalledWith('/auth/me', { credentials: 'same-origin' });
	});
});
