import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ServerHealth } from '@/lib/server-health/model';

const mocks = vi.hoisted(() => ({
	invoke: vi.fn(),
	browser: vi.fn(() => true),
	t1: vi.fn(() => false),
	principal: vi.fn((): { is_admin: boolean } | null => null),
}));

vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	invoke: mocks.invoke,
	isBrowserSession: mocks.browser,
}));
vi.mock('@/lib/transport/t1-session', async (orig) => ({
	...(await orig<typeof import('@/lib/transport/t1-session')>()),
	isT1Session: mocks.t1,
	currentPrincipal: mocks.principal,
}));

import { fetchServerHealth, mayAskForServerHealth } from '@/lib/queries/server-health';
import { ServerHealthCard, ServerHealthPanel } from './server-health-card';

const GB = 1024 ** 3;
const NOW = Date.parse('2026-10-08T12:00:00Z');

function snap(over: Partial<ServerHealth> = {}): ServerHealth {
	return {
		schema: 1,
		taken_at_ms: NOW,
		version: '0.22.0',
		tier: 't1',
		cpu_count: 2,
		load: { m1: 0.1, m5: 0.1, m15: 0.1 },
		memory: { total_bytes: 3.8 * GB, available_bytes: 0.3 * GB },
		swap: { total_bytes: 0, used_bytes: 0 },
		disk: { total_bytes: 80 * GB, free_bytes: 50 * GB },
		uptime_secs: 90_000,
		units: [
			{
				name: 'devotee-db-tunnel.service',
				kind: 'tunnel',
				active_state: 'failed',
				sub_state: 'failed',
				result: 'exit-code',
				last_trigger: null,
				next_elapse: null,
			},
		],
		accounts: [{ username: 'ned', running: true, terminals: 2, claude_processes: 1 }],
		unavailable: ['pressure', 'backups'],
		...over,
	};
}

function wrap(ui: React.ReactElement) {
	const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>);
}

beforeEach(() => {
	mocks.invoke.mockReset();
	mocks.browser.mockReturnValue(true);
	mocks.t1.mockReturnValue(false);
	mocks.principal.mockReturnValue(null);
});
afterEach(cleanup);

describe('who may see the card', () => {
	it('never on the desktop', () => {
		mocks.browser.mockReturnValue(false);
		expect(mayAskForServerHealth()).toBe(false);
	});
	it('a T0 browser (the owner) may', () => {
		expect(mayAskForServerHealth()).toBe(true);
	});
	it('a T1 admin may, a T1 member may not, and nobody before the principal is known', () => {
		mocks.t1.mockReturnValue(true);
		mocks.principal.mockReturnValue({ is_admin: true });
		expect(mayAskForServerHealth()).toBe(true);
		mocks.principal.mockReturnValue({ is_admin: false });
		expect(mayAskForServerHealth()).toBe(false);
		mocks.principal.mockReturnValue(null);
		expect(mayAskForServerHealth()).toBe(false);
	});
	it('a T1 member renders nothing and never calls the arm', async () => {
		mocks.t1.mockReturnValue(true);
		mocks.principal.mockReturnValue({ is_admin: false });
		const { container } = wrap(<ServerHealthPanel />);
		await Promise.resolve();
		expect(container.innerHTML).toBe('');
		expect(mocks.invoke).not.toHaveBeenCalled();
	});
	it('the desktop renders nothing and never calls the arm', async () => {
		mocks.browser.mockReturnValue(false);
		const { container } = wrap(<ServerHealthPanel />);
		await Promise.resolve();
		expect(container.innerHTML).toBe('');
		expect(mocks.invoke).not.toHaveBeenCalled();
	});
	it('an admin gets the card once the snapshot arrives', async () => {
		mocks.t1.mockReturnValue(true);
		mocks.principal.mockReturnValue({ is_admin: true });
		mocks.invoke.mockResolvedValue(snap());
		wrap(<ServerHealthPanel />);
		expect(await screen.findByTestId('server-health-card')).toBeTruthy();
		expect(mocks.invoke).toHaveBeenCalledWith('server_health');
	});
	it('a refused read (a share, a member) renders nothing', async () => {
		mocks.invoke.mockRejectedValue(new Error('forbidden'));
		const { container } = wrap(<ServerHealthPanel />);
		await new Promise((r) => setTimeout(r, 20));
		expect(container.innerHTML).toBe('');
	});
});

describe('fetchServerHealth', () => {
	it('refuses a snapshot from a newer schema rather than rendering it wrong', async () => {
		mocks.invoke.mockResolvedValue({ ...snap(), schema: 2 });
		expect(await fetchServerHealth()).toBeNull();
	});
});

describe('the card', () => {
	it('shows plain numbers and the status of each check', () => {
		render(<ServerHealthCard health={snap()} now={() => NOW + 3000} />);
		const mem = screen
			.getByTestId('server-health-card')
			.querySelector('[data-chip="memory"]') as HTMLElement;
		expect(mem.dataset.level).toBe('warn');
		expect(within(mem).getByText(/307 MB free of 3\.8 GB/)).toBeTruthy();
		const units = screen
			.getByTestId('server-health-card')
			.querySelector('[data-chip="units"]') as HTMLElement;
		expect(units.dataset.level).toBe('error');
		expect(screen.getByTestId('server-health-overall').dataset.level).toBe('error');
	});
	it('lists per-account counts only, and says what could not be measured', () => {
		render(<ServerHealthCard health={snap()} now={() => NOW} />);
		expect(screen.getByText('ned')).toBeTruthy();
		expect(screen.getByText('2 terminals · 1 claude')).toBeTruthy();
		expect(screen.getByTestId('server-health-unavailable').textContent).toMatch(
			/pressure, backups/
		);
	});
	it('says "All checks pass" when green', () => {
		render(
			<ServerHealthCard
				health={snap({
					memory: { total_bytes: 3.8 * GB, available_bytes: 3 * GB },
					swap: { total_bytes: 2 * GB, used_bytes: 0 },
					units: [],
					unavailable: [],
					backups: { enabled: true, updated: null, databases: [], schedules: [] },
				})}
				now={() => NOW}
			/>
		);
		expect(screen.getByTestId('server-health-overall').textContent).toMatch(/All checks pass/);
	});
});
