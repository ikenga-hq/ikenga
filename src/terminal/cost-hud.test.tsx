import { render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { CostHud, type StatuslineSnapshot } from './cost-hud';

const eventHandlers: Array<(payload: { payload: StatuslineSnapshot }) => void> = [];

vi.mock('@tauri-apps/api/event', () => ({
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
