import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { usePwaStore } from '@/lib/pwa/update-store';

const applyUpdate = vi.hoisted(() => vi.fn());
vi.mock('@/lib/pwa/register', () => ({ applyUpdate }));

import { PwaUpdateBanner } from './pwa-update-banner';

beforeEach(() => {
	applyUpdate.mockClear();
	usePwaStore.setState({ waiting: null });
});
afterEach(cleanup);

describe('<PwaUpdateBanner />', () => {
	it('renders nothing while no update is waiting', () => {
		const { container } = render(<PwaUpdateBanner />);
		expect(container.firstChild).toBeNull();
	});

	it('is a prominent, undismissable notice while an update waits, and reloads on the button', async () => {
		usePwaStore.setState({ waiting: {} as ServiceWorker });
		render(<PwaUpdateBanner />);
		const banner = screen.getByRole('status');
		expect(banner.getAttribute('data-state')).toBe('pwa-update');
		// Warning tone, not the quiet info tone of the slot's other notices.
		expect(banner.getAttribute('data-tone')).toBe('warning');
		expect(banner.textContent).toContain('A new version of Ikenga is ready.');
		// Nothing to dismiss: it stays until the page reloads.
		expect(screen.queryByRole('button', { name: /dismiss/i })).toBeNull();

		await userEvent.setup().click(screen.getByRole('button', { name: 'Reload to update' }));
		expect(applyUpdate).toHaveBeenCalledTimes(1);
	});

	it('stays up across store updates until the waiting worker is gone', () => {
		usePwaStore.setState({ waiting: {} as ServiceWorker });
		render(<PwaUpdateBanner />);
		expect(screen.queryByRole('status')).not.toBeNull();
		usePwaStore.setState({ installPrompt: null });
		expect(screen.queryByRole('status')).not.toBeNull();
	});
});
