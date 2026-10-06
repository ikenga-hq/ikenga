// Gap audit rank 14 — "Open file" links. In a browser session the daemon
// serves neither settings_open_file nor actions_open_file, so the link is
// hidden rather than offered as a silent no-op; on the desktop a failure is
// shown instead of swallowed by `.catch(() => {})`.

import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ remote: false }));

vi.mock('@/lib/tauri-cmd', () => ({
	canOpenFilesWithOs: () => !h.remote,
}));
vi.mock('@tanstack/react-router', () => ({
	Link: () => null,
}));

import { WritesNote } from './footer';

describe('WritesNote "Open file" (gap rank 14)', () => {
	afterEach(() => {
		cleanup();
		h.remote = false;
	});

	it('is hidden in a remote browser session and never calls the opener', () => {
		h.remote = true;
		const open = vi.fn(async () => '/x');
		render(<WritesNote stepId="welcome" onOpenFile={open} />);
		expect(screen.queryByTestId('onboarding-writes-open')).toBeNull();
		expect(open).not.toHaveBeenCalled();
	});

	it('calls the opener on the desktop', async () => {
		const open = vi.fn(async () => '/x');
		render(<WritesNote stepId="welcome" onOpenFile={open} />);
		await act(async () => {
			fireEvent.click(screen.getByTestId('onboarding-writes-open'));
		});
		expect(open).toHaveBeenCalledTimes(1);
		expect(screen.queryByTestId('onboarding-writes-open-error')).toBeNull();
	});

	it('shows a desktop failure instead of swallowing it', async () => {
		const open = vi.fn(async () => {
			throw new Error('no default editor');
		});
		render(<WritesNote stepId="welcome" onOpenFile={open} />);
		await act(async () => {
			fireEvent.click(screen.getByTestId('onboarding-writes-open'));
		});
		expect(screen.getByTestId('onboarding-writes-open-error').textContent).toMatch(
			/no default editor/
		);
	});
});
