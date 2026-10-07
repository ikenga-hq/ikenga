// honest-failure-states WP-2 — the pane banner per health state, and the
// D-5 / D-6 confirm copy.

import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { WslHealth } from '@/lib/tauri-cmd';

vi.mock('@/lib/wsl-health/fix-flow', () => ({
	requestWslFix: vi.fn(),
	confirmWslFix: vi.fn(() => Promise.resolve()),
}));
vi.mock('@/lib/wsl-health/query', () => ({
	probeWslHealth: vi.fn(() => Promise.resolve()),
	useWslHealth: vi.fn(() => ({ data: undefined })),
}));

import { WslFixConfirmBody } from './wsl-fix-dialog';
import { WslHealthBannerView } from './wsl-health-banner';

afterEach(cleanup);

function health(over: Partial<WslHealth>): WslHealth {
	return {
		state: 'ok',
		distro: null,
		detail: '',
		mirroredFailure: null,
		networkingMode: 'mirrored',
		checkedAt: 100,
		...over,
	};
}

function renderBanner(
	h: WslHealth,
	extra: Partial<Parameters<typeof WslHealthBannerView>[0]> = {}
) {
	const props = { onFix: vi.fn(), onCheck: vi.fn(), onDismiss: vi.fn(), run: null, ...extra };
	const r = render(<WslHealthBannerView health={h} {...props} />);
	return { ...r, props };
}

describe('WslHealthBannerView', () => {
	it('renders nothing when WSL is fine or not installed', () => {
		expect(renderBanner(health({ state: 'ok' })).container.firstChild).toBeNull();
		expect(renderBanner(health({ state: 'not_installed' })).container.firstChild).toBeNull();
	});

	it('no_route with a failed mirrored setup: cause + restart + NAT', async () => {
		const { props } = renderBanner(
			health({ state: 'no_route', mirroredFailure: { at: 1, errorCode: '0x8007054f' } })
		);
		expect(
			screen.getByText(
				"WSL started without a network connection — Windows couldn't set up mirrored networking (0x8007054f)"
			)
		).toBeTruthy();
		expect(screen.getByRole('alert')).toBeTruthy();
		await userEvent.click(
			screen.getByRole('button', { name: 'Restart WSL networking (needs admin)' })
		);
		expect(props.onFix).toHaveBeenCalledWith('restart_networking');
		await userEvent.click(screen.getByRole('button', { name: 'Switch to NAT…' }));
		expect(props.onFix).toHaveBeenCalledWith('switch_to_nat');
	});

	it('dns_only: Repair DNS', async () => {
		const { props } = renderBanner(health({ state: 'dns_only', distro: 'Ubuntu' }));
		await userEvent.click(screen.getByRole('button', { name: 'Repair DNS' }));
		expect(props.onFix).toHaveBeenCalledWith('repair_dns');
	});

	it('host_offline: no fix button, just the plain cause', () => {
		renderBanner(health({ state: 'host_offline' }));
		expect(screen.getByText('Your computer is offline')).toBeTruthy();
		const labels = screen
			.getAllByRole('button')
			.map((b) => b.textContent ?? b.getAttribute('aria-label'));
		expect(labels.some((l) => /Repair|Restart|NAT/.test(l ?? ''))).toBe(false);
	});

	it('Check again and dismiss call back', async () => {
		const { props } = renderBanner(health({ state: 'dns_only' }));
		await userEvent.click(screen.getByRole('button', { name: 'Check WSL network again' }));
		expect(props.onCheck).toHaveBeenCalled();
		await userEvent.click(screen.getByRole('button', { name: 'Hide until WSL is back online' }));
		expect(props.onDismiss).toHaveBeenCalled();
	});

	it('disables the fixes while one runs', () => {
		renderBanner(health({ state: 'dns_only' }), {
			run: { action: 'repair_dns', phase: 'running', message: null, at: 200 },
		});
		expect(screen.getByText('Working on it…')).toBeTruthy();
		expect((screen.getByRole('button', { name: 'Repair DNS' }) as HTMLButtonElement).disabled).toBe(
			true
		);
	});

	it("shows a fix's outcome only until a newer probe arrives", () => {
		const run = {
			action: 'restart_networking' as const,
			phase: 'cancelled' as const,
			message: 'Cancelled at the administrator prompt — nothing was changed.',
			at: 200,
		};
		renderBanner(health({ state: 'no_route', checkedAt: 100 }), { run });
		expect(screen.getByText(run.message)).toBeTruthy();
		cleanup();
		renderBanner(health({ state: 'no_route', checkedAt: 300 }), { run });
		expect(screen.queryByText(run.message)).toBeNull();
	});
});

describe('WslFixConfirmBody', () => {
	const sessions = [
		{
			tabId: 'a',
			title: 'claude',
			distro: 'Ubuntu',
			claudeSessionId: 's1',
			wasRunning: true,
			ephemeral: false,
		},
		{
			tabId: 'b',
			title: 'bash',
			distro: 'default',
			claudeSessionId: null,
			wasRunning: true,
			ephemeral: false,
		},
		{
			tabId: 'c',
			title: 'old',
			distro: 'default',
			claudeSessionId: null,
			wasRunning: false,
			ephemeral: false,
		},
	];

	it('restart lists the running WSL sessions it will close (D-5)', () => {
		const { container } = render(
			<WslFixConfirmBody confirm={{ action: 'restart_networking', distro: 'default', sessions }} />
		);
		expect(screen.getByText(/administrator approval/)).toBeTruthy();
		const items = container.querySelectorAll('[data-wsl-fix-sessions] li');
		expect(items).toHaveLength(2);
		expect(items[0].textContent).toContain('resumes');
	});

	it('NAT explains the LAN/Tailscale trade-off and the backup (D-6)', () => {
		render(
			<WslFixConfirmBody confirm={{ action: 'switch_to_nat', distro: 'Ubuntu', sessions: [] }} />
		);
		expect(screen.getByText(/LAN or Tailscale address/)).toBeTruthy();
		expect(screen.getByText(/\.wslconfig\.bak-/)).toBeTruthy();
		expect(screen.getByText(/nothing in Ikenga closes/)).toBeTruthy();
	});
});
