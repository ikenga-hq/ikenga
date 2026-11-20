// WP-06: a renderer whose read is refused for size shows the shared plate,
// not a raw error line.

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/transport', async (importOriginal) => ({
	...(await importOriginal<typeof import('@/lib/transport')>()),
	isBrowserHost: () => true,
	listen: vi.fn(async () => () => {}),
}));
vi.mock('@/lib/transport/fs-download', () => ({ downloadFile: vi.fn(async () => {}) }));
vi.mock('@/lib/tauri-cmd', () => ({
	fsRead: vi.fn(async () => {
		throw new Error('fs_read: preview_too_large: 99999999 > 26214400');
	}),
}));
vi.mock('react-pdf', () => ({
	pdfjs: { GlobalWorkerOptions: {} },
	Document: () => null,
	Page: () => null,
}));

import { ImageView } from './image-view';
import { PdfView } from './pdf-view';
import { XlsxView } from './xlsx-view';

afterEach(cleanup);

describe.each([
	['ImageView', () => <ImageView path="/srv/a.png" />],
	['PdfView', () => <PdfView path="/srv/a.pdf" />],
	['XlsxView', () => <XlsxView path="/srv/a.xlsx" />],
])('%s over the preview cap', (_n, el) => {
	it('shows the too-large plate with a Download button', async () => {
		render(el());
		expect(await screen.findByText(/too large to preview/)).toBeTruthy();
		expect(screen.getByRole('button', { name: 'Download' })).toBeTruthy();
	});
});
