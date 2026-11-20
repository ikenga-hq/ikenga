import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	browser: true,
	downloadFile: vi.fn(async (_p: string) => {}),
}));

vi.mock('@/lib/transport', () => ({ isBrowserHost: () => h.browser }));
vi.mock('@/lib/transport/fs-download', () => ({ downloadFile: h.downloadFile }));

import { CannotPreviewPlate } from './cannot-preview-plate';

beforeEach(() => {
	h.browser = true;
	h.downloadFile.mockClear();
});
afterEach(cleanup);

describe('CannotPreviewPlate', () => {
	it('shows a custom reason with Download instead of the size line', () => {
		render(<CannotPreviewPlate path="/srv/a.heic" reason="HEIC images can't be previewed here." />);
		expect(screen.getByText(/HEIC images can't be previewed/)).toBeTruthy();
		expect(screen.queryByText(/too large/)).toBeNull();
		expect(screen.getByRole('button', { name: 'Download' })).toBeTruthy();
	});

	it('says why, with the size, and Download uses the shared function', async () => {
		render(
			<CannotPreviewPlate
				path="/srv/big/video.mov"
				message={new Error('fs_read: preview_too_large: 209715200 > 26214400')}
			/>
		);
		expect(screen.getByText('video.mov')).toBeTruthy();
		expect(screen.getByText(/200\.0 MB is too large to preview/)).toBeTruthy();
		fireEvent.click(screen.getByRole('button', { name: 'Download' }));
		expect(h.downloadFile).toHaveBeenCalledWith('/srv/big/video.mov');
	});

	it('desktop keeps the "Open in default app" label', () => {
		h.browser = false;
		render(<CannotPreviewPlate path="/home/u/big.bin" size={30 * 1024 * 1024} />);
		expect(screen.getByRole('button', { name: 'Open in default app' })).toBeTruthy();
	});
});
