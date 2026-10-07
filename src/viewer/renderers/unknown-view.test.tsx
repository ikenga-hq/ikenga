import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { dismissToast, useToastStore } from '@/lib/toast';

const host = vi.hoisted(() => ({
	browser: false,
	openLocalPath: vi.fn(async () => {}),
}));

vi.mock('@/lib/transport', () => ({
	isBrowserHost: () => host.browser,
	openLocalPath: host.openLocalPath,
}));
vi.mock('@/lib/tauri-cmd', () => ({
	fsRead: vi.fn(async () => ({ bytes: [1, 2, 3], mime: 'application/zip' })),
}));

import { UnknownView } from './unknown-view';

beforeEach(() => {
	host.browser = false;
	host.openLocalPath.mockReset();
	host.openLocalPath.mockResolvedValue(undefined);
	while (useToastStore.getState().queue.length) dismissToast();
});
afterEach(cleanup);

describe('UnknownView — open in default app', () => {
	it('desktop: hands the path to the OS opener', async () => {
		render(<UnknownView path="/home/u/a.zip" />);
		fireEvent.click(screen.getByRole('button', { name: 'Open in default app' }));
		expect(host.openLocalPath).toHaveBeenCalledWith('/home/u/a.zip', { kind: 'file' });
	});

	it('browser: the button is a download, never a window.open of the path', async () => {
		host.browser = true;
		const open = vi.spyOn(window, 'open').mockReturnValue(null);
		render(<UnknownView path="/srv/a.zip" />);
		fireEvent.click(screen.getByRole('button', { name: 'Download' }));
		expect(host.openLocalPath).toHaveBeenCalledWith('/srv/a.zip', { kind: 'file' });
		expect(open).not.toHaveBeenCalled();
		open.mockRestore();
	});

	it('reports a failed open with a toast', async () => {
		host.openLocalPath.mockRejectedValue(new Error('no such file'));
		render(<UnknownView path="/srv/gone.zip" />);
		fireEvent.click(screen.getByRole('button', { name: 'Open in default app' }));
		await waitFor(() =>
			expect(useToastStore.getState().queue[0]?.label).toBe('Could not open gone.zip: no such file')
		);
	});
});
