import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/terminal/single-terminal', () => ({ createTerminalSession: vi.fn(() => 's1') }));
vi.mock('@/lib/panes/pane-store', () => ({
	usePaneStore: Object.assign(() => null, {
		getState: () => ({ focusedId: 'p1', addTab: vi.fn(), placeView: vi.fn() }),
	}),
}));

import { useTerminalStore } from '@/terminal/session-store';
import { SessionsSection } from './sessions';

afterEach(() => {
	cleanup();
	useTerminalStore.setState({ tabs: [], restoreError: null });
});

describe('SessionsSection restore notice', () => {
	it('an unreadable saved list is an error row offering Resume saving', () => {
		useTerminalStore.setState({
			tabs: [],
			restoreError: { message: "Couldn't restore your previous terminals", holdsSave: true },
		});
		render(<SessionsSection {...({} as Parameters<typeof SessionsSection>[0])} />);
		const row = screen.getByTestId('explorer-section-error');
		expect(row.getAttribute('data-tone')).toBe('error');
		expect(row.getAttribute('role')).toBe('alert');
		expect(screen.getByText('Resume saving')).toBeDefined();
	});

	it('the "saving resumed, copied to …" notice is shown as info, not as an error', () => {
		useTerminalStore.setState({
			tabs: [],
			restoreError: {
				message:
					'Saving resumed. The unreadable terminal list was copied to "terminal.tabs.unreadable-1".',
				holdsSave: false,
				backupKey: 'terminal.tabs.unreadable-1',
			},
		});
		render(<SessionsSection {...({} as Parameters<typeof SessionsSection>[0])} />);
		const row = screen.getByTestId('explorer-section-error');
		expect(row.getAttribute('data-tone')).toBe('info');
		expect(row.getAttribute('role')).toBe('status');
		expect(screen.getByText('Dismiss')).toBeDefined();
	});
});
