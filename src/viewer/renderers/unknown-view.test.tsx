import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { dismissToast, useToastStore } from '@/lib/toast';

const host = vi.hoisted(() => ({
	browser: false,
	openLocalPath: vi.fn(async (_path: string, _opts?: unknown) => {}),
}));

vi.mock('@/lib/transport', () => ({
	isBrowserHost: () => host.browser,
}));
// The shared download: desktop hands the path to the OS opener, a browser downloads.
vi.mock('@/lib/transport/fs-download', () => ({
	downloadFile: (path: string) => host.openLocalPath(path, { kind: 'file' }),
}));
const rpc = vi.hoisted(() => ({
	fsStat: vi.fn(async (_p: string) => ({ size: 3, isDir: false, modifiedMs: 0, mime: 'application/zip' })),
	fsRead: vi.fn(async () => {
		throw new Error('unknown view must stat, not read');
	}),
}));
vi.mock('@/lib/tauri-cmd', () => ({
	fsStat: rpc.fsStat,
	fsRead: rpc.fsRead,
}));

import { UnknownView } from './unknown-view';

beforeEach(() => {
	host.browser = false;
	host.openLocalPath.mockReset();
	host.openLocalPath.mockResolvedValue(undefined);
	while (useToastStore.getState().queue.length) dismissToast();
});
afterEach(cleanup);

describe('UnknownView — size', () => {
	it('learns the size from a stat and never reads the file', async () => {
		render(<UnknownView path="/home/u/big.docx" />);
		await screen.findByText(/application\/zip · 3 B/);
		expect(rpc.fsStat).toHaveBeenCalledWith('/home/u/big.docx');
		expect(rpc.fsRead).not.toHaveBeenCalled();
	});
});

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
