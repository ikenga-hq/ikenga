// plans/pwa S1 (W2): the honest offline state.

import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ServerUnreachable } from './server-unreachable';

afterEach(() => {
	cleanup();
	vi.restoreAllMocks();
});

describe('<ServerUnreachable />', () => {
	it('says the server is unreachable, and retries on click', async () => {
		const onRetry = vi.fn();
		render(<ServerUnreachable onRetry={onRetry} />);
		expect(screen.getByRole('alert').textContent).toMatch(/Can't reach the Ikenga server/);
		await userEvent.setup().click(screen.getByRole('button', { name: 'Retry' }));
		expect(onRetry).toHaveBeenCalledTimes(1);
	});

	it('names an offline device as offline, and retries when the network returns', () => {
		vi.spyOn(navigator, 'onLine', 'get').mockReturnValue(false);
		const onRetry = vi.fn();
		render(<ServerUnreachable onRetry={onRetry} />);
		expect(screen.getByRole('alert').textContent).toMatch(/This device is offline/);
		act(() => {
			window.dispatchEvent(new Event('online'));
		});
		expect(onRetry).toHaveBeenCalledTimes(1);
		expect(screen.getByRole('alert').textContent).not.toMatch(/This device is offline/);
	});
});
