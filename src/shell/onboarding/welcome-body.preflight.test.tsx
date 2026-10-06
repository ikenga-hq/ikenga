// Welcome step preflight, rendered per session kind.
//
// The daemon does not serve `detect_system` (desktop_only.toml, WP-19), so a
// browser session that called it showed "Detection failed — Command
// 'detect_system' not implemented in headless daemon". These tests drive the
// real transport — Tauri `invoke` on desktop, `fetch('/api/rpc')` +
// `/api/health` in a browser — with a fake daemon that answers exactly like
// the real one, including that refusal, so a regression to the desktop call
// fails here the way it failed for users.
//
// The T1 case is the one earlier bugs came from: a cookie session holds no
// bearer token, and code that read "no token" as "no backend" went down the
// desktop path.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, within } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
	tauriInvoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.tauriInvoke }));

vi.mock('@tanstack/react-router', () => ({
	Link: ({
		children,
		to,
		...props
	}: {
		children: ReactNode;
		to: string;
		'data-testid'?: string;
	}) => (
		<a href={to} data-testid={props['data-testid']}>
			{children}
		</a>
	),
}));

const TOKEN_KEY = 'ikenga_auth_token';
const NOT_SERVED = "Command 'detect_system' not implemented in headless daemon";

const fetchMock = vi.fn<typeof fetch>();
/** Every `/api/rpc` command the page sent, in order. */
let rpcCalls: string[] = [];

function json(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json' },
	});
}

/** A fake `ikenga-server` at `tier`: `/api/health`, `/auth/me`, and an
 *  `/api/rpc` that serves `list_claude_projects` and refuses `detect_system`
 *  with the daemon's own wording. */
function fakeDaemon(tier: 't0' | 't1') {
	fetchMock.mockImplementation(async (input, init) => {
		const url = typeof input === 'string' ? input : (input as Request).url;
		if (url === '/api/health') {
			return json({
				ok: true,
				name: 'ikenga-server',
				version: '0.9.1',
				status: 'ready',
				uptime_secs: 7_500,
				executor: { tier, pty: true, piped: true, principal_isolation: tier === 't1' },
				...(tier === 't1' ? { probe: { ok: true, at: 1_790_000_000 } } : {}),
			});
		}
		if (url === '/auth/me') {
			return json({ principal_id: 'p-1', username: 'ada', is_admin: false });
		}
		if (url === '/api/rpc') {
			const { cmd } = JSON.parse(String(init?.body)) as { cmd: string };
			rpcCalls.push(cmd);
			if (cmd === 'list_claude_projects') {
				return json({ ok: true, data: [{ slug: 'a' }, { slug: 'b' }] });
			}
			return json({ ok: false, error: `Command '${cmd}' not implemented in headless daemon` });
		}
		return json({ error: 'not found' }, 404);
	});
}

/** Fresh transport + T1 flag + component, one module registry. */
async function freshWelcome() {
	vi.resetModules();
	const t1 = await import('@/lib/transport/t1-session');
	const { WelcomeBody } = await import('./welcome-body');
	return { t1, WelcomeBody };
}

function renderWelcome(WelcomeBody: (p: { onContinue: () => void }) => ReactNode) {
	const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return render(
		<QueryClientProvider client={client}>
			<WelcomeBody onContinue={() => {}} />
		</QueryClientProvider>
	);
}

async function rows() {
	const list = await screen.findByTestId('preflight-list');
	return within(list)
		.getAllByTestId('preflight-row')
		.map((r) => ({
			id: r.getAttribute('data-check-id'),
			level: r.getAttribute('data-level'),
			text: r.textContent ?? '',
		}));
}

// The first import of the welcome tree transforms a lot of modules; do it
// once up front so no single test pays for it against its timeout.
beforeAll(async () => {
	await import('./welcome-body');
}, 60_000);

beforeEach(() => {
	sessionStorage.clear();
	localStorage.clear();
	window.history.replaceState(null, '', '/');
	rpcCalls = [];
	mocks.tauriInvoke.mockReset();
	fetchMock.mockReset();
	vi.stubGlobal('fetch', fetchMock);
	delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
});

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
	delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
});

describe('WelcomeBody preflight — desktop', () => {
	it('runs detect_system over Tauri and renders its rows unchanged', async () => {
		(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
		mocks.tauriInvoke.mockImplementation(async (cmd: string) => {
			if (cmd === 'detect_system') {
				return {
					os: 'linux',
					arch: 'x86_64',
					disk_free_gb: 120,
					app_data_dir: '/home/u/.local/share/app.ikenga',
					app_data_writable: true,
					secrets_store_ready: true,
					vault_key_present: true,
					claude_projects_dir_present: true,
					checks: [
						{ id: 'os', level: 'pass', message: 'Linux 6.8', fix_hint: null },
						{ id: 'vault', level: 'pass', message: 'Keychain available', fix_hint: null },
					],
				};
			}
			throw new Error(`unexpected ${cmd}`);
		});
		const { WelcomeBody } = await freshWelcome();
		renderWelcome(WelcomeBody);

		const got = await rows();
		expect(got.map((r) => r.id)).toEqual(['os', 'vault']);
		expect(got[1].text).toContain('Stronghold vault');
		expect(mocks.tauriInvoke).toHaveBeenCalledWith('detect_system');
		expect(fetchMock).not.toHaveBeenCalled();

		expect(screen.getByText('System preflight')).toBeTruthy();
		expect(screen.queryByTestId('preflight-server-note')).toBeNull();
		expect(screen.getByTestId('welcome-restore-link')).toBeTruthy();
		expect(screen.getByTestId('onboarding-writes-open')).toBeTruthy();
		expect(screen.getByText(/Secrets sit in Stronghold/)).toBeTruthy();
	});
});

describe('WelcomeBody preflight — browser', () => {
	it('T0 (token tab): builds the server report, never asks for detect_system', async () => {
		sessionStorage.setItem(TOKEN_KEY, 't0-token');
		fakeDaemon('t0');
		const { WelcomeBody } = await freshWelcome();
		renderWelcome(WelcomeBody);

		const got = await rows();
		expect(got.map((r) => r.id)).toEqual(['server', 'access', 'sessions', 'claude_projects']);
		expect(got.every((r) => r.level === 'pass')).toBe(true);
		expect(got[0].text).toContain('ikenga-server 0.9.1 is ready · up 2h 5m');
		expect(got[1].text).toContain('access token');
		expect(got[2].text).toContain('Single-user server');
		expect(got[3].text).toContain('2 Claude Code projects on the server');

		expect(rpcCalls).toEqual(['list_claude_projects']);
		expect(mocks.tauriInvoke).not.toHaveBeenCalled();
		expect(screen.queryByTestId('preflight-error')).toBeNull();
		expect(screen.getByText('Server preflight')).toBeTruthy();
	});

	it('T1 (cookie, no token): server report with the signed-in user, no detect_system', async () => {
		fakeDaemon('t1');
		const { t1, WelcomeBody } = await freshWelcome();
		// What `detectBrowserTier` records at boot on a T1 server.
		await t1.detectT1Server();
		expect(t1.isT1Session()).toBe(true);
		expect(sessionStorage.getItem(TOKEN_KEY)).toBeNull();

		renderWelcome(WelcomeBody);

		const got = await rows();
		expect(got.map((r) => r.id)).toEqual(['server', 'access', 'sessions', 'claude_projects']);
		expect(got[1].text).toContain('Signed in as ada');
		expect(got[2].text).toContain('run as your own account');
		expect(got[2].text).toContain('isolation checked 2026-09-21');
		expect(rpcCalls).not.toContain('detect_system');
		expect(document.body.textContent).not.toContain(NOT_SERVED);
		expect(screen.queryByTestId('preflight-error')).toBeNull();

		// Desktop-only affordances are gone; no vault/keychain copy.
		expect(screen.queryByTestId('welcome-restore-link')).toBeNull();
		expect(screen.queryByTestId('onboarding-writes-open')).toBeNull();
		expect(document.body.textContent).not.toMatch(/Stronghold|keychain|vault/i);
		expect(screen.getByTestId('preflight-server-note')).toBeTruthy();

		// Continue is open: warnings/passes only.
		const cont = screen.getByTestId('welcome-inline-continue') as HTMLButtonElement;
		expect(cont.disabled).toBe(false);
		t1.__setT1SessionForTests(false, null);
	});

	it('shows the health failure itself when the server cannot be read', async () => {
		sessionStorage.setItem(TOKEN_KEY, 't0-token');
		fetchMock.mockImplementation(async () => json({}, 502));
		const { WelcomeBody } = await freshWelcome();
		renderWelcome(WelcomeBody);

		const err = await screen.findByTestId('preflight-error');
		expect(err.textContent).toContain('Server health check failed (HTTP 502)');
	});
});
