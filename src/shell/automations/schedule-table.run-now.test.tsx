// Gap audit rank 23 — "Run now" used to swallow a failed mutation (no onError),
// so the click looked like it did nothing. The failure now shows next to it.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ runNow: vi.fn() }));

vi.mock('@/lib/tauri-cmd', () => ({
	agentOpsRunNow: h.runNow,
	agentOpsSetEnabled: vi.fn(),
}));

import type { AutomationRow } from './types';
import { ScheduleTable } from './schedule-table';

const ROW = {
	id: 'r1',
	name: 'Nightly',
	source: 'agent-ops',
	agentOpsJobId: 'job-1',
	paused: false,
	filePath: '/x/jobs.json',
	cronWords: 'every night',
	cronExpr: '0 0 * * *',
	target: 'skill',
	engine: 'claude',
	lastRun: '—',
	nextRun: '—',
	runNowDisabledReason: null,
	pauseDisabledReason: null,
	editDisabledReason: null,
} as unknown as AutomationRow;

function renderTable() {
	const qc = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
	render(
		<QueryClientProvider client={qc}>
			<ScheduleTable rows={[ROW]} onOpenHistory={() => {}} onOpenEdit={() => {}} />
		</QueryClientProvider>
	);
}

afterEach(() => {
	cleanup();
	h.runNow.mockReset();
});

describe('Run now error surfacing (gap rank 23)', () => {
	it('shows the honest reason when the daemon does not serve the command', async () => {
		h.runNow.mockRejectedValue(
			new Error("Command 'agent_ops_run_now' not implemented in headless daemon")
		);
		renderTable();
		await userEvent.setup().click(screen.getByRole('button', { name: /Run now/ }));
		await waitFor(() =>
			expect(screen.getByTestId('run-now-error').textContent).toBe(
				'Not available on this server yet'
			)
		);
	});

	it('shows a real failure as itself', async () => {
		h.runNow.mockResolvedValue({ ok: false, code: 'E_BUSY', error: 'already running' });
		renderTable();
		await userEvent.setup().click(screen.getByRole('button', { name: /Run now/ }));
		await waitFor(() =>
			expect(screen.getByTestId('run-now-error').textContent).toContain('already running')
		);
	});
});
