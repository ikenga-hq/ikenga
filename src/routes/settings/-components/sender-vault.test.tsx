// WP-K — Settings › Integrations › Sender (box): who sees what, the
// write-only password inputs, and the test-then-save flow.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { VaultResult, VaultStatus } from '@/lib/transport/sender-vault';
import { __setT1SessionForTests } from '@/lib/transport/t1-session';

import { SenderVaultCards, SenderVaultSection } from './sender-vault';

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
	__setT1SessionForTests(false);
	delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
});

const SECRET = 'hunter2-canary';

function status(over: Partial<VaultStatus> = {}): VaultStatus {
	return {
		person: 'ADAUGO',
		admin: false,
		groups: [
			{
				group: 'outreach:ADAUGO',
				label: 'Outreach mailbox (ADAUGO)',
				shared: false,
				names: [],
				present: { OUTREACH_ADAUGO_SMTP_HOST: 16, OUTREACH_ADAUGO_SMTP_PASS: 14 },
				missing: ['OUTREACH_ADAUGO_IMAP_PASS'],
				complete: false,
				lastChanged: '2026-10-30T10:00:00.000Z',
				lastChangedBy: 'ada',
				readiness: { identityKnown: true, approverListed: false },
			},
		],
		...over,
	};
}

function ok<T>(data: T): VaultResult<T> {
	return { ok: true, data };
}

function setup(s: VaultStatus, admin: boolean, replies: VaultResult<unknown>[] = []) {
	const api = vi.fn(async () => replies.shift() ?? ok({ group: 'x', test: 'ok', tested: 'login' }));
	const onChanged = vi.fn();
	render(<SenderVaultCards status={s} admin={admin} api={api as never} onChanged={onChanged} />);
	return { api, onChanged, user: userEvent.setup() };
}

describe('visibility', () => {
	it('a member sees only their own mailbox card', () => {
		setup(status(), false);
		expect(screen.getByText('My outreach mailbox')).toBeTruthy();
		for (const g of ['ruby_mail', 'buffer', 'listmonk', 'cms_draft', 'tasks']) {
			expect(screen.queryByTestId(`vault-${g}`)).toBeNull();
		}
	});

	it('an admin (account and sender agree) also sees the five shared cards', () => {
		setup(status({ admin: true }), true);
		for (const g of ['ruby_mail', 'buffer', 'listmonk', 'cms_draft', 'tasks']) {
			expect(screen.getByTestId(`vault-${g}`)).toBeTruthy();
		}
	});

	it('a member whose sender view says admin still gets no shared cards without the account role', () => {
		setup(status({ admin: true }), false);
		expect(screen.queryByTestId('vault-buffer')).toBeNull();
	});

	it('says so when the account has no mailbox slot', () => {
		setup(status({ person: null, groups: [] }), false);
		expect(screen.getByTestId('vault-no-slot')).toBeTruthy();
	});

	function mount() {
		const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
		return render(
			<QueryClientProvider client={qc}>
				<SenderVaultSection />
			</QueryClientProvider>
		);
	}

	it('renders nothing on the desktop and never asks', () => {
		(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
		__setT1SessionForTests(true, { principal_id: 'p', username: 'ada', is_admin: false });
		const fetchMock = vi.fn();
		vi.stubGlobal('fetch', fetchMock);
		const { container } = mount();
		expect(container.textContent).toBe('');
		expect(fetchMock).not.toHaveBeenCalled();
	});

	it('renders nothing on a T0 server', () => {
		__setT1SessionForTests(false);
		const fetchMock = vi.fn();
		vi.stubGlobal('fetch', fetchMock);
		const { container } = mount();
		expect(container.textContent).toBe('');
		expect(fetchMock).not.toHaveBeenCalled();
	});

	it('on the box it asks the server (POST, cookie) and shows the member card', async () => {
		__setT1SessionForTests(true, { principal_id: 'p', username: 'ada', is_admin: false });
		const fetchMock = vi.fn<typeof fetch>(
			async () =>
				new Response(JSON.stringify({ ok: true, data: status() }), {
					status: 200,
					headers: { 'Content-Type': 'application/json' },
				})
		);
		vi.stubGlobal('fetch', fetchMock);
		mount();
		await screen.findByText('My outreach mailbox');
		const [url, init] = fetchMock.mock.calls[0];
		expect(url).toBe('/api/sender/vault/status');
		expect(init?.method).toBe('POST');
		expect(init?.credentials).toBe('same-origin');
		expect(screen.queryByTestId('vault-buffer')).toBeNull();
	});
});

describe('write-only inputs', () => {
	it('password inputs are type=password, autocomplete new-password, and empty even when set', () => {
		setup(status(), false);
		const pw = screen.getByTestId('field-OUTREACH_ADAUGO_SMTP_PASS') as HTMLInputElement;
		expect(pw.type).toBe('password');
		expect(pw.getAttribute('autocomplete')).toBe('new-password');
		expect(pw.value).toBe('');
		const host = screen.getByTestId('field-OUTREACH_ADAUGO_SMTP_HOST') as HTMLInputElement;
		expect(host.value).toBe('');
		expect(host.getAttribute('autocomplete')).toBe('off');
		// Ports default to 465 / 993.
		expect((screen.getByTestId('field-OUTREACH_ADAUGO_SMTP_PORT') as HTMLInputElement).value).toBe(
			'465'
		);
		expect((screen.getByTestId('field-OUTREACH_ADAUGO_IMAP_PORT') as HTMLInputElement).value).toBe(
			'993'
		);
		// Status shows a length, not a value.
		expect(screen.getByTestId('vault-outreach-status').textContent).toContain(
			'set (14 characters)'
		);
	});
});

async function fillMailbox(user: ReturnType<typeof userEvent.setup>) {
	const f = (k: string, v: string) =>
		user.type(screen.getByTestId(`field-OUTREACH_ADAUGO_${k}`), v);
	await f('SMTP_HOST', 'smtp.example.com');
	await f('SMTP_USER', 'ada@example.com');
	await f('SMTP_PASS', SECRET);
	await f('IMAP_HOST', 'imap.example.com');
	await f('IMAP_USER', 'ada@example.com');
	await f('IMAP_PASS', SECRET);
}

describe('save flow', () => {
	it('saves with one set call (the sender tests first), clears the passwords from the inputs and reports', async () => {
		const { api, onChanged, user } = setup(status(), false, [
			ok({ group: 'outreach:ADAUGO', names: [], replaced: false, test: 'ok', tested: 'login' }),
		]);
		await fillMailbox(user);
		await user.click(screen.getByTestId('vault-outreach-save'));
		await waitFor(() => expect(onChanged).toHaveBeenCalled());
		expect(api.mock.calls.map((c) => c[0])).toEqual(['set']);
		const [, body] = api.mock.calls[0] as unknown as [
			string,
			{ group: string; values: Record<string, string> },
		];
		expect(body.group).toBe('outreach:ADAUGO');
		expect(body.values.OUTREACH_ADAUGO_SMTP_PASS).toBe(SECRET);
		expect(body.values.OUTREACH_ADAUGO_SMTP_PORT).toBe('465');
		for (const k of ['SMTP_PASS', 'IMAP_PASS']) {
			expect((screen.getByTestId(`field-OUTREACH_ADAUGO_${k}`) as HTMLInputElement).value).toBe('');
		}
		expect(screen.getByTestId('vault-outreach-message').textContent).toContain(
			'login check passed'
		);
		expect(document.body.innerHTML).not.toContain(SECRET);
	});

	it('a failed login test saves nothing and says why in plain words', async () => {
		const { api, user } = setup(status(), false, [
			{ ok: false, code: 'test_failed', message: 'x', test: 'smtp_auth_failed', tested: 'login' },
		]);
		await fillMailbox(user);
		await user.click(screen.getByTestId('vault-outreach-save'));
		const msg = await screen.findByTestId('vault-outreach-message');
		expect(msg.textContent).toContain('rejected the sending (SMTP) login');
		expect(msg.textContent).toContain('Nothing was saved');
		expect(api.mock.calls.map((c) => c[0])).toEqual(['set']);
		// Even on failure the password is gone from the input.
		expect((screen.getByTestId('field-OUTREACH_ADAUGO_SMTP_PASS') as HTMLInputElement).value).toBe(
			''
		);
	});

	it('shows "format checked only" for a shape_only save', async () => {
		const { user } = setup(status({ admin: true }), true, [
			ok({ group: 'cms_draft', names: [], replaced: false, test: 'ok', tested: 'shape_only' }),
		]);
		const card = screen.getByTestId('vault-cms_draft');
		await user.type(within(card).getByTestId('field-CMS_MCP_AUTHORIZATION'), 'Bearer abc');
		await user.click(within(card).getByTestId('vault-cms_draft-save'));
		const msg = await within(card).findByTestId('vault-cms_draft-message');
		expect(msg.textContent).toContain('Format checked only');
	});

	it('refuses an empty required field without calling the server', async () => {
		const { api, user } = setup(status(), false);
		await user.click(screen.getByTestId('vault-outreach-save'));
		expect(api).not.toHaveBeenCalled();
		expect((await screen.findByTestId('vault-outreach-message')).textContent).toContain(
			'SMTP host is empty'
		);
	});
});

describe('clear', () => {
	it('asks in-app first and only then clears', async () => {
		const { api, onChanged, user } = setup(status(), false, [
			ok({ group: 'outreach:ADAUGO', names: ['OUTREACH_ADAUGO_SMTP_HOST'] }),
		]);
		await user.click(screen.getByTestId('vault-outreach-clear'));
		expect(api).not.toHaveBeenCalled();
		expect(await screen.findByText(/Clear My outreach mailbox\?/)).toBeTruthy();
		await user.click(screen.getByTestId('vault-outreach-clear-confirm'));
		await waitFor(() => expect(onChanged).toHaveBeenCalled());
		expect(api).toHaveBeenCalledWith('clear', { group: 'outreach:ADAUGO' });
	});
});
