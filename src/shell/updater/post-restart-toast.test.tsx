import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const consumePendingRestartIfMatching = vi.fn();
const useLiveSessionCount = vi.fn();

vi.mock('@/lib/updater/post-restart', () => ({
	consumePendingRestartIfMatching: () => consumePendingRestartIfMatching(),
}));
vi.mock('@/lib/updater/restart-sessions', () => ({
	useLiveSessionCount: () => useLiveSessionCount(),
}));

import { PostRestartUpdateToast } from './post-restart-toast';

beforeEach(() => {
	consumePendingRestartIfMatching.mockReset();
	useLiveSessionCount.mockReset().mockReturnValue(0);
});
afterEach(cleanup);

describe('<PostRestartUpdateToast />', () => {
	it('renders nothing when there is no matching marker', async () => {
		consumePendingRestartIfMatching.mockResolvedValue(null);
		const { container } = render(<PostRestartUpdateToast />);
		await waitFor(() => expect(consumePendingRestartIfMatching).toHaveBeenCalled());
		expect(container.textContent).toBe('');
	});

	it('shows the version and the resumed-session count when a marker matches', async () => {
		consumePendingRestartIfMatching.mockResolvedValue({
			version: '0.9.1',
			notes: '',
			sessionsBefore: 2,
			at: Date.now(),
		});
		useLiveSessionCount.mockReturnValue(2);

		render(<PostRestartUpdateToast />);

		await waitFor(() => expect(screen.getByText('0.9.1')).toBeTruthy());
		expect(screen.getByText(/2 of 2 session/)).toBeTruthy();
	});

	it('caps the reported resume count at what actually came back', async () => {
		consumePendingRestartIfMatching.mockResolvedValue({
			version: '0.9.1',
			notes: '',
			sessionsBefore: 2,
			at: Date.now(),
		});
		// One of the two didn't make it back — never claim more than liveNow.
		useLiveSessionCount.mockReturnValue(1);

		render(<PostRestartUpdateToast />);

		await waitFor(() => expect(screen.getByText(/1 of 2 session/)).toBeTruthy());
	});
});
