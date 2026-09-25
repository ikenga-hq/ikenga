// WP-41-F0 regression: the download progress segment must show its label as
// visible text, not only in a hover `title` attribute.

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { useUpdaterStore } from '@/lib/updater/updater-store';
import { UpdaterStatusBarProgress } from './status-bar-slot';

const INITIAL_STATE = useUpdaterStore.getState();

afterEach(() => {
	cleanup();
	useUpdaterStore.setState(INITIAL_STATE, true);
});

describe('UpdaterStatusBarProgress', () => {
	it('renders the version and percentage as visible text while downloading', () => {
		useUpdaterStore.setState({
			installing: true,
			available: { version: '0.9.1' },
			bytesDownloaded: 280,
			totalBytes: 1000,
		});

		render(<UpdaterStatusBarProgress engine="claude-code" />);

		// Visible text — not just the `title` supplement.
		expect(screen.getByText(/Downloading 0\.9\.1/)).toBeDefined();
		expect(screen.getByText(/28%/)).toBeDefined();
	});

	it('falls back to the read-only engine segment when no download is active', () => {
		useUpdaterStore.setState({ installing: false });

		render(<UpdaterStatusBarProgress engine="claude-code" />);

		expect(screen.getByText('claude-code')).toBeDefined();
		expect(screen.queryByText(/Downloading/)).toBeNull();
	});
});
