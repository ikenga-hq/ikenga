import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const host = vi.hoisted(() => ({
	downloadFile: vi.fn(async (_path: string) => {}),
}));

vi.mock('@/lib/transport', () => ({
	isBrowserHost: () => true,
}));
vi.mock('@/lib/transport/fs-download', () => ({
	downloadFile: host.downloadFile,
}));
vi.mock('@/lib/tauri-cmd', () => ({
	fsRead: vi.fn(async () => ({ bytes: [37, 80, 68, 70, 45], mime: 'application/pdf' })),
}));
vi.mock('react-pdf', () => ({
	pdfjs: { GlobalWorkerOptions: {} },
	Document: () => null,
	Page: () => null,
}));

import { PdfView } from './pdf-view';

beforeEach(() => host.downloadFile.mockClear());
afterEach(cleanup);

describe('PdfView — download', () => {
	it('calls the shared download with the file path (the server streams the real bytes)', async () => {
		render(<PdfView path="/srv/home/deliverables/report.pdf" />);
		fireEvent.click(await screen.findByRole('button', { name: 'Download' }));
		expect(host.downloadFile).toHaveBeenCalledTimes(1);
		expect(host.downloadFile).toHaveBeenCalledWith('/srv/home/deliverables/report.pdf');
	});
});
