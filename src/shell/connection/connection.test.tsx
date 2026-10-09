import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { EMPTY_RTT, summarizeRtt } from '@/lib/connection/rtt';

vi.mock('@/lib/queries/server-health', () => ({ mayAskForServerHealth: () => true }));
vi.mock('@/lib/connection/rtt-monitor', () => ({
	mayMeasureConnection: () => true,
	useConnectionRtt: () => EMPTY_RTT,
}));

import { ConnectionPanelBody } from './connection-panel';
import {
	ConnectionSegmentBody,
	connectionTitle,
	hasConnectionReading,
	shortRtt,
} from './connection-segment';

afterEach(cleanup);

const steady = (ms: number) => summarizeRtt([ms, ms + 10, ms - 10, ms + 10, ms], 0, 'ws');

describe('ConnectionPanelBody', () => {
	it('shows "340 ms ± 85"-style figures and a typing explanation', () => {
		render(<ConnectionPanelBody summary={steady(340)} />);
		expect(screen.getByTestId('connection-figure').textContent).toMatch(/^340 ms ± \d+$/);
		expect(screen.getByTestId('connection-level').dataset.connLevel).toBe('bad');
		expect(screen.getByTestId('connection-explain').textContent).toMatch(/Typing will lag/);
	});
	it('is good below 150 ms', () => {
		render(<ConnectionPanelBody summary={steady(80)} />);
		expect(screen.getByTestId('connection-level').dataset.connLevel).toBe('good');
	});
	it('shows the Server link only when given one (admins)', () => {
		const open = vi.fn();
		const { rerender } = render(<ConnectionPanelBody summary={steady(80)} />);
		expect(screen.queryByText('Server health')).toBeNull();
		rerender(<ConnectionPanelBody summary={steady(80)} onOpenServer={open} />);
		fireEvent.click(screen.getByRole('button', { name: 'Open' }));
		expect(open).toHaveBeenCalledOnce();
	});
	it('says Measuring before the first sample', () => {
		render(<ConnectionPanelBody summary={EMPTY_RTT} />);
		expect(screen.getByTestId('connection-figure').textContent).toBe('Measuring…');
	});
});

describe('status bar segment', () => {
	it('has nothing to show until a reading exists', () => {
		expect(hasConnectionReading(EMPTY_RTT)).toBe(false);
		expect(hasConnectionReading(steady(100))).toBe(true);
		expect(hasConnectionReading(summarizeRtt([], 1, null))).toBe(true);
	});
	it('prints the median in ms, or "no reply" when stalled', () => {
		expect(shortRtt(steady(240))).toBe('240 ms');
		expect(shortRtt(summarizeRtt([100, 100, 100], 2, 'ws'))).toBe('no reply');
	});
	it('the tooltip carries the figure and the typing explanation', () => {
		const t = connectionTitle(steady(240));
		expect(t).toMatch(/^Connection: 240 ms ± \d+\./);
		expect(t).toMatch(/slightly delayed/);
	});
	it('colours amber then red', () => {
		const { container, rerender } = render(<ConnectionSegmentBody summary={steady(100)} />);
		expect(container.innerHTML).toContain('--success');
		rerender(<ConnectionSegmentBody summary={steady(200)} />);
		expect(container.innerHTML).toContain('--warning');
		rerender(<ConnectionSegmentBody summary={steady(400)} />);
		expect(container.innerHTML).toContain('--danger');
	});
});
