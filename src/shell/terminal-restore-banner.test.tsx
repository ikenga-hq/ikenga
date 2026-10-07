// The unreadable-saved-terminals notice must be visible app-wide while
// saving is paused, not only inside the Explorer's Sessions section.

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { useTerminalStore } from '@/terminal/session-store';
import { TerminalRestoreBanner } from './terminal-restore-banner';

afterEach(cleanup);
beforeEach(() => {
	localStorage.clear();
	useTerminalStore.setState({ restoreError: null });
});

describe('<TerminalRestoreBanner />', () => {
	it('shows while saving is held; Resume saving backs up first and says where', async () => {
		localStorage.setItem('terminal.tabs', '{boom');
		useTerminalStore.setState({
			restoreError: { message: "Couldn't restore your previous terminals: boom.", holdsSave: true },
		});
		render(<TerminalRestoreBanner />);
		expect(screen.getByTestId('terminal-restore-banner').textContent).toMatch(/boom/);
		fireEvent.click(screen.getByTestId('terminal-restore-banner-resume'));
		await waitFor(() =>
			expect(screen.getByTestId('terminal-restore-banner').textContent).toMatch(
				/copied to "terminal\.tabs\.unreadable-\d+"/
			)
		);
		expect(useTerminalStore.getState().restoreError?.holdsSave).toBe(false);
		fireEvent.click(screen.getByTestId('terminal-restore-banner-dismiss'));
		expect(useTerminalStore.getState().restoreError).toBeNull();
		expect(screen.queryByTestId('terminal-restore-banner')).toBeNull();
	});

	it('stays out of the way for a notice that does not hold saving', () => {
		useTerminalStore.setState({
			restoreError: { message: 'resume setting unreadable', holdsSave: false },
		});
		const { container } = render(<TerminalRestoreBanner />);
		expect(container.textContent).toBe('');
	});
});
