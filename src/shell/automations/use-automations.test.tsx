// Regression coverage for WP-42-F0 (timezone round-trip) and WP-42-F1 (honest
// pause-disabled reason) — the review's own suggested tests
// (2026-09-25-5b-conformance-review.md, WP-42-F8).

import { renderHook, waitFor } from '@testing-library/react';
import { describe, expect, it, vi, afterEach } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import * as tauriCmd from '@/lib/tauri-cmd';
import { useAutomations } from './use-automations';

vi.mock('@/lib/tauri-cmd', () => ({
	agentOpsListJobs: vi.fn(),
	pkgKernelStatus: vi.fn(),
	pkgPreviewManifest: vi.fn(),
}));

afterEach(() => {
	vi.resetAllMocks();
});

function wrapper({ children }: { children: ReactNode }) {
	const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
}

const KERNEL_STATUS = {
	api_version: 5,
	registries: {},
	installed: [],
} as never;

function agentOpsJob(overrides: Record<string, unknown>) {
	return {
		id: 'weekly-digest',
		label: 'Weekly digest',
		schedule: '0 9 * * 1',
		schedule_dialect: '5f',
		timezone: 'UTC',
		enabled: true,
		command: 'release-status',
		mode: 'agent',
		state: {},
		...overrides,
	};
}

describe('useAutomations — WP-42 review fixes', () => {
	it('round-trips a non-default timezone from AgentOpsRawJob onto the row (WP-42-F0)', async () => {
		vi.mocked(tauriCmd.pkgKernelStatus).mockResolvedValue(KERNEL_STATUS);
		vi.mocked(tauriCmd.agentOpsListJobs).mockResolvedValue({
			ok: true,
			daemon_up: true,
			jobs: [agentOpsJob({ timezone: 'Africa/Lagos' })],
		} as never);

		const { result } = renderHook(() => useAutomations(), { wrapper });

		await waitFor(() => expect(result.current.rows).toHaveLength(1));
		expect(result.current.rows[0].timezone).toBe('Africa/Lagos');
	});

	it('keeps Pause/Resume enabled for agent-ops rows when the daemon is down (WP-42-F1)', async () => {
		vi.mocked(tauriCmd.pkgKernelStatus).mockResolvedValue(KERNEL_STATUS);
		vi.mocked(tauriCmd.agentOpsListJobs).mockResolvedValue({
			ok: true,
			daemon_up: false,
			jobs: [agentOpsJob({})],
		} as never);

		const { result } = renderHook(() => useAutomations(), { wrapper });

		await waitFor(() => expect(result.current.rows).toHaveLength(1));
		const row = result.current.rows[0];
		// A config-file write (agent_ops_set_enabled) never contacts the daemon.
		expect(row.pauseDisabledReason).toBeNull();
		// Run now genuinely needs the live daemon (HTTP trigger) — stays disabled.
		expect(row.runNowDisabledReason).not.toBeNull();
	});
});
