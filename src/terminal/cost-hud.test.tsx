import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { CostHud, type StatuslineSnapshot } from './cost-hud';

const h = vi.hoisted(() => ({ remote: false }));

const eventHandlers: Array<(payload: { payload: StatuslineSnapshot }) => void> = [];

vi.mock('@/lib/transport', () => ({
	isRemoteWebSession: () => h.remote,
	listen: vi.fn((_channel: string, handler: (event: { payload: StatuslineSnapshot }) => void) => {
		eventHandlers.push(handler);
		return Promise.resolve(() => {});
	}),
}));

vi.mock('@/lib/iyke/client', () => ({
	// Return a non-ok response so the component stays in the event-driven
	// path and the test can assert per-terminal filtering.
	iykeFetch: vi.fn().mockResolvedValue({ ok: false }),
}));

afterEach(cleanup);

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

// Gap audit rank 10 stopgap — a browser session never receives
// `statusline://snapshot`, so "listening" would wait forever.
describe('CostHud in a remote session', () => {
	it('says telemetry is not available in the browser instead of listening', () => {
		h.remote = true;
		try {
			render(<CostHud sessionId="term-a" />);
			expect(
				screen.getByText("Statusline telemetry isn't available in the browser yet")
			).toBeDefined();
			expect(screen.queryByText(/listening for statusline telemetry/i)).toBeNull();
		} finally {
			h.remote = false;
		}
	});
});
