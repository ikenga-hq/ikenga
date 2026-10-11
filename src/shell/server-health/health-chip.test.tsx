import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { queryKeys } from '@/lib/query-keys';
import type { ServerHealth } from '@/lib/server-health/model';

const mocks = vi.hoisted(() => ({
	invoke: vi.fn(),
	browser: vi.fn(() => true),
	t1: vi.fn(() => true),
	principal: vi.fn((): { is_admin: boolean } | null => ({ is_admin: true })),
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

import { SERVER_HEALTH_CHIP_POLL_MS } from '@/lib/queries/server-health';
import { ServerHealthChipBody, summarizeHealth, useServerHealthChip } from './health-chip';

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
		memory: { total_bytes: 4 * GB, available_bytes: 2 * GB },
		swap: { total_bytes: 2 * GB, used_bytes: 0 },
		disk: { total_bytes: 80 * GB, free_bytes: 50 * GB },
		unavailable: [],
		...over,
	};
}

function Probe() {
	const chip = useServerHealthChip();
	if (!chip) return <div data-testid="none" />;
	return (
		<button type="button" aria-label={chip.aria} data-level={chip.level}>
			<ServerHealthChipBody chip={chip} />
		</button>
	);
}

function mount(qc = new QueryClient()) {
	return {
		qc,
		...render(
			<QueryClientProvider client={qc}>
				<Probe />
			</QueryClientProvider>
		),
	};
}

beforeEach(() => {
	mocks.invoke.mockReset();
	mocks.browser.mockReturnValue(true);
	mocks.t1.mockReturnValue(true);
	mocks.principal.mockReturnValue({ is_admin: true });
});
afterEach(cleanup);

describe('visibility', () => {
	it('is hidden on the desktop and never asks the server', async () => {
		mocks.browser.mockReturnValue(false);
		mount();
		expect(screen.getByTestId('none')).toBeTruthy();
		expect(mocks.invoke).not.toHaveBeenCalled();
	});
	it('is hidden for a non-admin member', () => {
		mocks.principal.mockReturnValue({ is_admin: false });
		mount();
		expect(screen.getByTestId('none')).toBeTruthy();
		expect(mocks.invoke).not.toHaveBeenCalled();
	});
	it('shows for an admin browser session', async () => {
		mocks.invoke.mockResolvedValue(snap());
		mount();
		const b = await screen.findByRole('button', { name: /Server ok/i });
		expect(b.getAttribute('data-level')).toBe('ok');
		expect(b.textContent).toBe('Server');
	});
});

describe('level mapping', () => {
	it('ok', () => {
		const v = summarizeHealth(snap(), NOW);
		expect(v.level).toBe('ok');
		expect(v.label).toBe('Server');
		expect(v.tooltip).toMatch(/CPU load.*memory 50% used.*disk 38% used.*last backup/);
	});
	it('warn names the worst reason', () => {
		const v = summarizeHealth(snap({ memory: { total_bytes: 100, available_bytes: 12 } }), NOW);
		expect(v.level).toBe('warn');
		expect(v.label).toBe('Memory 88%');
		expect(v.aria).toContain('Memory 88%');
	});
	it('critical for disk', () => {
		const v = summarizeHealth(snap({ disk: { total_bytes: 100, free_bytes: 4 } }), NOW);
		expect(v.level).toBe('error');
		expect(v.label).toBe('Disk 96%');
	});
	it('critical for a failed backup, and it outranks a warning', () => {
		const v = summarizeHealth(
			snap({
				memory: { total_bytes: 100, available_bytes: 12 },
				backups: {
					enabled: true,
					updated: null,
					schedules: [],
					databases: [
						{
							name: 'main',
							schedule: 'daily',
							last_attempt: null,
							last_attempt_ok: false,
							last_success: null,
							last_error_kind: 'dump',
							last_error_at: null,
							bytes: null,
							duration_s: null,
						},
					],
				},
			}),
			NOW
		);
		expect(v.level).toBe('error');
		expect(v.label).toBe('Backup failed');
	});
	it('unknown (grey, label still Server) when nothing could be read', () => {
		const v = summarizeHealth(null);
		expect(v.level).toBe('unknown');
		expect(v.label).toBe('Server');
	});
	it('an RPC failure renders grey with no toast', async () => {
		mocks.invoke.mockRejectedValue(new Error('boom'));
		mount();
		const b = await screen.findByRole('button', { name: /unknown/i });
		expect(b.getAttribute('data-level')).toBe('unknown');
	});
});

describe('query', () => {
	it('shares the card cache key', async () => {
		mocks.invoke.mockResolvedValue(snap());
		const { qc } = mount();
		await screen.findByRole('button');
		const cached = qc.getQueryData(queryKeys.serverHealth.snapshot()) as ServerHealth;
		expect(cached.version).toBe('0.22.0');
		expect(mocks.invoke).toHaveBeenCalledTimes(1);
	});
	it('polls every 60 s and not in the background', async () => {
		mocks.invoke.mockResolvedValue(snap());
		const { qc } = mount();
		await waitFor(() => expect(mocks.invoke).toHaveBeenCalled());
		const obs = qc.getQueryCache().find({ queryKey: queryKeys.serverHealth.snapshot() })
			?.observers[0];
		expect(SERVER_HEALTH_CHIP_POLL_MS).toBe(60_000);
		expect(obs?.options.refetchInterval).toBe(60_000);
		expect(obs?.options.refetchIntervalInBackground).toBe(false);
	});
});
