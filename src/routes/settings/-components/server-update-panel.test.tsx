// WP-P9 — Settings › About › Server updates: the confirm sends the count it
// showed, a `terminals_open` refusal re-opens it with the new count, the
// button explains why it is disabled, progress and outcome render, and the
// log tail is collapsible.

import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { serverUpdateRun, serverUpdateView } from '@/lib/transport/server-update.fixtures';
import type { ApplyServerUpdateResult } from '@/lib/transport/server-update';

vi.mock('@/lib/actions/runner/open', () => ({ gotoRoute: vi.fn() }));

import { ServerUpdateCard } from './server-update-panel';

afterEach(cleanup);

function setup(
	view = serverUpdateView(),
	results: ApplyServerUpdateResult[] = [{ ok: true, version: '0.21.0', requestId: 'r-1' }],
	reloadNeeded = false
) {
	const apply = vi.fn(
		async () => results.shift() ?? { ok: true as const, version: '0.21.0', requestId: 'r' }
	);
	const onChanged = vi.fn();
	render(
		<ServerUpdateCard view={view} reloadNeeded={reloadNeeded} apply={apply} onChanged={onChanged} />
	);
	return { apply, onChanged, user: userEvent.setup() };
}

describe('<ServerUpdateCard />', () => {
	it('shows current → available, release notes and the terminal warning', () => {
		setup(serverUpdateView({ open_terminals: 2, open_terminals_partial: true }));
		const panel = screen.getByTestId('server-update-panel');
		expect(panel.textContent).toContain('v0.20.0');
		expect(screen.getByTestId('server-update-available').textContent).toBe('v0.21.0 available');
		const notes = screen.getByRole('link', { name: /Release notes/ });
		expect(notes.getAttribute('href')).toBe(
			'https://github.com/ikenga-hq/ikenga/releases/tag/v0.21.0'
		);
		expect(notes.getAttribute('rel')).toContain('noopener');
		expect(screen.getByTestId('server-update-terminals').textContent).toBe(
			'Updating restarts it and ends at least 2 open terminals. (count may be incomplete)'
		);
	});

	it('the confirm restates the version and count, and sends the count it showed', async () => {
		const { apply, onChanged, user } = setup(serverUpdateView({ open_terminals: 2 }));
		await user.click(screen.getByRole('button', { name: /Update now/ }));
		const dialog = await screen.findByRole('dialog');
		expect(dialog.textContent).toContain('Update the server to Ikenga 0.21.0?');
		expect(dialog.textContent).toContain('2 open terminals will end');
		await user.click(within(dialog).getByRole('button', { name: /Update and restart/ }));
		expect(apply).toHaveBeenCalledWith({ version: '0.21.0', acknowledgedOpenTerminals: 2 });
		expect(onChanged).toHaveBeenCalled();
		expect(screen.queryByRole('dialog')).toBeNull();
	});

	it('re-opens the confirm with the new count after a terminals_open refusal', async () => {
		const { apply, user } = setup(serverUpdateView({ open_terminals: 2 }), [
			{ ok: false, code: 'terminals_open', message: 'ends 5', openTerminals: 5 },
			{ ok: true, version: '0.21.0', requestId: 'r-2' },
		]);
		await user.click(screen.getByRole('button', { name: /Update now/ }));
		await user.click(
			within(await screen.findByRole('dialog')).getByRole('button', { name: /Update and restart/ })
		);
		const dialog = await screen.findByRole('dialog');
		expect(dialog.textContent).toContain('5 open terminals will end');
		expect(within(dialog).getByRole('alert').textContent).toContain(
			'More terminals are open now (5)'
		);
		await user.click(within(dialog).getByRole('button', { name: /Update and restart/ }));
		expect(apply).toHaveBeenLastCalledWith({ version: '0.21.0', acknowledgedOpenTerminals: 5 });
	});

	it('shows why other refusals happened', async () => {
		const { user } = setup(serverUpdateView(), [
			{ ok: false, code: 'cooldown', message: 'raw server text' },
		]);
		await user.click(screen.getByRole('button', { name: /Update now/ }));
		await user.click(
			within(await screen.findByRole('dialog')).getByRole('button', { name: /Update and restart/ })
		);
		expect(screen.queryByRole('dialog')).toBeNull();
		expect(screen.getByRole('alert').textContent).toBe(
			'This version failed less than an hour ago; try again later.'
		);
	});

	it('disables Update now with its reason when the server says it cannot apply', () => {
		setup(serverUpdateView({ can_apply: false, apply_blocked_reason: 'cooldown' }));
		const button = screen.getByRole('button', { name: /Update now/ }) as HTMLButtonElement;
		expect(button.disabled).toBe(true);
		expect(screen.getByTestId('server-update-reason').textContent).toBe(
			'This version failed less than an hour ago; try again later.'
		);
		expect(button.parentElement?.getAttribute('title')).toBe(
			'This version failed less than an hour ago; try again later.'
		);
	});

	it('explains a blocked release (min_upgrade_from) instead of offering it', () => {
		const view = serverUpdateView({
			can_apply: false,
			apply_blocked_reason: 'blocked_min_upgrade',
		});
		view.available = { ...view.available!, blocked: true, min_upgrade_from: '0.20.5' };
		setup(view);
		expect(screen.getByTestId('server-update-blocked').textContent).toBe(
			'This release requires v0.20.5 first; update over SSH.'
		);
		expect(screen.queryByTestId('server-update-terminals')).toBeNull();
	});

	it('shows progress while root runs the update', () => {
		setup(
			serverUpdateView({
				can_apply: false,
				apply_blocked_reason: 'running',
				last_run: serverUpdateRun({ state: 'running', finished_at: null }),
			})
		);
		expect(screen.getByTestId('server-update-running').textContent).toMatch(
			/Updating to v0\.21\.0/
		);
		expect(screen.getByTestId('server-update-run-state').textContent).toBe('Running');
	});

	it('renders the outcome with a collapsible log tail', () => {
		setup(
			serverUpdateView({
				last_run: serverUpdateRun({
					state: 'rolled_back',
					rolled_back: true,
					exit_code: 3,
					message: 'health check failed; rolled back',
				}),
			})
		);
		const run = screen.getByTestId('server-update-last-run');
		expect(screen.getByTestId('server-update-run-state').textContent).toBe('Rolled back');
		expect(run.textContent).toContain('v0.20.0 → v0.21.0');
		expect(run.textContent).toContain('rolled back');
		expect(run.textContent).toContain('by ada');
		const log = screen.getByTestId('server-update-log');
		expect(log.closest('details')).not.toBeNull();
		expect(log.closest('details')?.open).toBe(false);
		expect(log.textContent).toContain('==> Upgrading 0.20.0 -> 0.21.0');
	});

	it('offers a reload once the server runs the new version', () => {
		setup(
			serverUpdateView({ current: '0.21.0', available: null, last_run: serverUpdateRun() }),
			[],
			true
		);
		expect(screen.getByRole('button', { name: 'Reload' })).toBeTruthy();
		expect(screen.queryByRole('button', { name: /Update now/ })).toBeNull();
	});
});
