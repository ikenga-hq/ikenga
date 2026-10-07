// Audit 2026-10-06 rank 17: a browser session said "Ikenga is up to date.
// Last checked just now" although the in-app updater is desktop-only and the
// check never ran there.

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { UpdateStatus } from './update-status';

afterEach(cleanup);

describe('UpdateStatus', () => {
	it('never claims "up to date" in a browser session', () => {
		render(
			<UpdateStatus desktop={false} available={false} checking={false} lastCheckedAt={Date.now()} />
		);
		expect(screen.queryByText(/up to date/i)).toBeNull();
		expect(screen.queryByText(/last checked/i)).toBeNull();
		expect(screen.getByTestId('update-status-browser').textContent).toBe(
			"This browser session can't install app updates. The server is updated by its administrator."
		);
	});

	it('points an admin who can update the server at the Server updates panel', () => {
		render(
			<UpdateStatus
				desktop={false}
				serverUpdates
				available={false}
				checking={false}
				lastCheckedAt={Date.now()}
			/>
		);
		const text = screen.getByTestId('update-status-browser').textContent ?? '';
		expect(text).toMatch(/see Server updates below/);
		expect(text).not.toMatch(/up to date/i);
	});

	it('keeps the desktop "up to date" line after a check', () => {
		render(<UpdateStatus desktop available={false} checking={false} lastCheckedAt={Date.now()} />);
		expect(screen.getByTestId('update-status-current').textContent).toBe(
			'Ikenga is up to date. Last checked just now.'
		);
	});

	it('shows nothing on the desktop while checking or when an update is available', () => {
		const { container, rerender } = render(
			<UpdateStatus desktop available={false} checking lastCheckedAt={null} />
		);
		expect(container.textContent).toBe('');
		rerender(<UpdateStatus desktop available checking={false} lastCheckedAt={null} />);
		expect(container.textContent).toBe('');
	});
});
