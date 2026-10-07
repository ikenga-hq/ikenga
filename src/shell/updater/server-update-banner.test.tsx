// WP-P9 — the server-update banner: hidden unless the viewer may update (the
// query only ever has data for a browser admin / the T0 operator), the
// available copy with the terminal count, the running and outcome states,
// and the per-version snooze.

import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { serverUpdateRun, serverUpdateView } from '@/lib/transport/server-update.fixtures';
import type { ServerUpdateView } from '@/lib/transport/server-update';

const { state, gotoRoute } = vi.hoisted(() => ({
	state: { data: null as ServerUpdateView | null, reload: false },
	gotoRoute: vi.fn(),
}));

vi.mock('@/lib/queries/server-update', () => ({
	useServerUpdate: () => ({ data: state.data }),
	needsReload: () => state.reload,
}));
vi.mock('@/lib/actions/runner/open', () => ({ gotoRoute }));

import { ServerUpdateBanner, terminalsCopy } from './server-update-banner';

beforeEach(() => {
	state.data = null;
	state.reload = false;
	gotoRoute.mockReset();
	localStorage.clear();
});
afterEach(cleanup);

function bannerState(): string | null {
	return document.querySelector('[data-state]')?.getAttribute('data-state') ?? null;
}

describe('<ServerUpdateBanner />', () => {
	it('renders nothing without a view (desktop, a member, an unsupported host)', () => {
		const { container } = render(<ServerUpdateBanner />);
		expect(container.textContent).toBe('');
	});

	it('renders nothing when the server is current or the update is blocked', () => {
		state.data = serverUpdateView({ available: null, apply_blocked_reason: 'none_available' });
		const { container, rerender } = render(<ServerUpdateBanner />);
		expect(container.textContent).toBe('');
		state.data = serverUpdateView();
		state.data.available!.blocked = true;
		rerender(<ServerUpdateBanner />);
		expect(container.textContent).toBe('');
	});

	it('shows the version and the terminals a restart ends, and links to the panel', async () => {
		state.data = serverUpdateView({ open_terminals: 3 });
		render(<ServerUpdateBanner />);
		expect(bannerState()).toBe('server-update-available');
		const text = screen.getByRole('status').textContent ?? '';
		expect(text).toContain('Ikenga 0.21.0');
		expect(text).toContain('is available for this server.');
		expect(text).toContain('Updating restarts it and ends 3 open terminals.');
		await userEvent.setup().click(screen.getByRole('button', { name: 'Review update' }));
		expect(gotoRoute).toHaveBeenCalledWith('/settings/about');
	});

	it('says so when no terminals are open, and hedges a partial count', () => {
		expect(terminalsCopy(0, false)).toBe('No terminals are open.');
		expect(terminalsCopy(1, false)).toBe('Updating restarts it and ends 1 open terminal.');
		expect(terminalsCopy(2, true)).toBe('Updating restarts it and ends at least 2 open terminals.');
	});

	it('Later snoozes that version only', async () => {
		state.data = serverUpdateView();
		const { container, rerender } = render(<ServerUpdateBanner />);
		await userEvent.setup().click(screen.getByRole('button', { name: 'Later' }));
		expect(container.textContent).toBe('');
		// A newer release is news again.
		state.data = serverUpdateView();
		state.data.available!.version = '0.22.0';
		rerender(<ServerUpdateBanner />);
		expect(bannerState()).toBe('server-update-available');
	});

	it('shows progress while root runs the update, and while a request waits', () => {
		state.data = serverUpdateView({
			can_apply: false,
			apply_blocked_reason: 'running',
			last_run: serverUpdateRun({ state: 'running', finished_at: null }),
		});
		const { rerender } = render(<ServerUpdateBanner />);
		expect(bannerState()).toBe('server-update-running');
		expect(screen.getByRole('status').textContent).toContain(
			'Updating the server to Ikenga 0.21.0'
		);

		state.data = serverUpdateView({
			can_apply: false,
			apply_blocked_reason: 'pending',
			pending_request: {
				version: '0.21.0',
				request_id: 'r',
				requested_by: 'ada',
				requested_at: null,
			},
		});
		rerender(<ServerUpdateBanner />);
		expect(bannerState()).toBe('server-update-pending');
	});

	it('warns about a rollback, and the warning can be dismissed for that run', async () => {
		state.data = serverUpdateView({
			last_run: serverUpdateRun({ state: 'rolled_back', rolled_back: true, exit_code: 3 }),
			can_apply: false,
			apply_blocked_reason: 'cooldown',
		});
		render(<ServerUpdateBanner />);
		expect(bannerState()).toBe('server-update-rolled-back');
		expect(screen.getByRole('status').textContent).toContain(
			'The update to Ikenga 0.21.0 failed its health check and was rolled back to 0.20.0.'
		);
		await act(async () => {
			await userEvent.setup().click(screen.getByRole('button', { name: 'Dismiss' }));
		});
		// Dismissed: back to the plain available notice.
		expect(bannerState()).toBe('server-update-available');
	});

	it('a failed run is an alert pointing at Settings › About', () => {
		state.data = serverUpdateView({
			last_run: serverUpdateRun({ state: 'failed', exit_code: 4 }),
		});
		render(<ServerUpdateBanner />);
		expect(bannerState()).toBe('server-update-failed');
		expect(screen.getByRole('alert').textContent).toContain('the server may need attention');
	});

	it('an old outcome is history, not news', () => {
		const old = new Date(Date.now() - 3 * 24 * 3600 * 1000).toISOString();
		state.data = serverUpdateView({
			last_run: serverUpdateRun({ state: 'rolled_back', started_at: old, finished_at: old }),
		});
		render(<ServerUpdateBanner />);
		expect(bannerState()).toBe('server-update-available');
	});

	it('offers a reload once the server runs the new version', () => {
		state.data = serverUpdateView({
			current: '0.21.0',
			available: null,
			last_run: serverUpdateRun(),
		});
		state.reload = true;
		render(<ServerUpdateBanner />);
		expect(bannerState()).toBe('server-update-reload');
		expect(screen.getByRole('button', { name: 'Reload' })).toBeTruthy();
	});
});
