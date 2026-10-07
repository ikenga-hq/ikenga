import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { __resetTerminalHooksForTests } from '@/lib/iyke/terminal-hooks';
import { CostHud, type StatuslineSnapshot } from './cost-hud';

const h = vi.hoisted(() => ({
	remote: false,
	snapshots: {} as Record<string, unknown>,
	info: { settingsDir: '/d/term-hooks', reason: null } as {
		settingsDir: string | null;
		reason: string | null;
	},
	infoCalls: 0,
}));

const eventHandlers: Array<(payload: { payload: StatuslineSnapshot }) => void> = [];

vi.mock('@/lib/transport', () => ({
	isRemoteWebSession: () => h.remote,
	listen: vi.fn((_channel: string, handler: (event: { payload: StatuslineSnapshot }) => void) => {
		eventHandlers.push(handler);
		return Promise.resolve(() => {});
	}),
}));

vi.mock('@/lib/tauri-cmd', () => ({
	termHooksStatuslineSnapshot: vi.fn(async () => h.snapshots),
	termHooksInfo: vi.fn(async () => {
		h.infoCalls += 1;
		return h.info;
	}),
	termHooksDecide: vi.fn(),
}));

vi.mock('@/lib/iyke/client', () => ({
	// Return a non-ok response so the component stays in the event-driven
	// path and the test can assert per-terminal filtering.
	iykeFetch: vi.fn().mockResolvedValue({ ok: false }),
}));

afterEach(() => {
	cleanup();
	__resetTerminalHooksForTests();
});

describe('CostHud per-terminal filtering', () => {
	it('shows only events for its own session id', async () => {
		render(
			<div>
				<CostHud sessionId="term-a" />
				<CostHud sessionId="term-b" />
			</div>
		);

		// Both start in the listening state.
		expect(screen.getAllByText(/listening for statusline telemetry/i).length).toBe(2);

		// Wait for both components to attach their listeners
		await waitFor(() => {
			expect(eventHandlers.length).toBeGreaterThanOrEqual(2);
		});

		// Fire a statusline event for term-a only.
		for (const h of eventHandlers) {
			h({
				payload: {
					ikenga_terminal_id: 'term-a',
					model: { id: 'claude-sonnet-4-20250514', display_name: 'Claude Sonnet' },
					cost: { total_cost_usd: 0.123 },
					context_window: { used_percentage: 42 },
				},
			});
		}

		// term-a should show the data; term-b should still be listening.
		await waitFor(() => {
			expect(screen.getByText(/CTX: 42%/i)).toBeDefined();
			expect(screen.getByText(/0\.123/i)).toBeDefined();
		});
		expect(screen.getByText(/listening for statusline telemetry/i)).toBeDefined();
	});
});

// A browser session's HUD is fed by the daemon (gap audit rank 11): its
// snapshots are read through the daemon's arm and arrive on `/ws/events`. Only
// a server that can't take claude's statusline says so.
describe('CostHud in a remote session', () => {
	it('reads the daemon snapshot map and shows this terminal in it', async () => {
		h.remote = true;
		h.snapshots = {
			'term-a': { model: { display_name: 'Claude Remote' }, cost: { total_cost_usd: 1.25 } },
		};
		try {
			render(<CostHud sessionId="term-a" />);
			await waitFor(() => expect(screen.getByText('Claude Remote')).toBeDefined());
			expect(screen.getByText(/1\.250/)).toBeDefined();
		} finally {
			h.remote = false;
			h.snapshots = {};
		}
	});

	it("says why when the server can't take claude's statusline, and no longer claims it is unavailable in the browser", async () => {
		h.remote = true;
		h.info = {
			settingsDir: null,
			reason: 'Not available on this server: it runs without a data folder',
		};
		try {
			render(<CostHud sessionId="term-a" />);
			await waitFor(() =>
				expect(
					screen.getByText(
						'Statusline telemetry: Not available on this server: it runs without a data folder'
					)
				).toBeDefined()
			);
			expect(screen.queryByText(/isn't available in the browser yet/)).toBeNull();
		} finally {
			h.remote = false;
			h.info = { settingsDir: '/d/term-hooks', reason: null };
			__resetTerminalHooksForTests();
		}
	});

	it('listens while the server can take it and nothing has arrived yet', async () => {
		h.remote = true;
		try {
			render(<CostHud sessionId="term-a" />);
			await waitFor(() => expect(h.infoCalls).toBeGreaterThan(0));
			expect(screen.getByText(/listening for statusline telemetry/i)).toBeDefined();
		} finally {
			h.remote = false;
			__resetTerminalHooksForTests();
		}
	});
});
