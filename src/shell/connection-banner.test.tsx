// G-ACCESS §3.10 (WP-78a): the connection banner names a deliberate close —
// "Signed out" (4401) or "Access changed" (4403) — instead of counting down a
// reconnect, and goes away once a socket is back in.

import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { connectionStateStore } from '@/lib/transport/connection-state';
import { ConnectionBanner } from './connection-banner';

beforeEach(() => connectionStateStore.__reset());
afterEach(() => {
	cleanup();
	connectionStateStore.__reset();
});

describe('ConnectionBanner access states', () => {
	it('renders nothing while connected', () => {
		const { container } = render(<ConnectionBanner />);
		expect(container.textContent).toBe('');
	});

	it('4401: "Signed out"', () => {
		render(<ConnectionBanner />);
		act(() => connectionStateStore.accessLost('revoked', 'device_revoked'));
		const banner = screen.getByTestId('connection-banner');
		expect(banner.getAttribute('data-state')).toBe('connection-access-revoked');
		expect(banner.textContent).toContain('Signed out');
	});

	it('4403: "Access changed", cleared when a socket reconnects', () => {
		const { container } = render(<ConnectionBanner />);
		act(() => connectionStateStore.accessLost('caps_changed'));
		expect(screen.getByTestId('connection-banner').getAttribute('data-state')).toBe(
			'connection-access-changed'
		);
		act(() => connectionStateStore.socketConnected('term-1'));
		expect(container.textContent).toBe('');
	});
});
