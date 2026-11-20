import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const host = vi.hoisted(() => ({
	saveBlobAs: vi.fn(),
	openLocalPath: vi.fn(async () => {}),
}));

vi.mock('@/lib/transport', () => ({
	isBrowserHost: () => true,
	openLocalPath: host.openLocalPath,
	saveBlobAs: host.saveBlobAs,
}));
vi.mock('@/lib/tauri-cmd', () => ({
	fsRead: vi.fn(async () => ({ bytes: [37, 80, 68, 70, 45], mime: 'application/pdf' })),
}));
// Stand in for pdf.js: like the real getDocument, it transfers the buffer it is
// handed to a worker, which detaches the caller's array.
vi.mock('react-pdf', () => ({
	pdfjs: { GlobalWorkerOptions: {} },
	Document: ({ file }: { file: { data: Uint8Array } | null }) => {
		if (file && file.data.byteLength > 0) {
			structuredClone(file.data, { transfer: [file.data.buffer as ArrayBuffer] });
		}
		return null;
	},
	Page: () => null,
}));

import { PdfView } from './pdf-view';

beforeEach(() => host.saveBlobAs.mockReset());
afterEach(cleanup);

describe('PdfView — download in a browser session', () => {
	it('saves the full PDF after pdf.js has taken its copy of the bytes', async () => {
		render(<PdfView path="/srv/home/deliverables/report.pdf" />);
		fireEvent.click(await screen.findByRole('button', { name: 'Download' }));
		expect(host.saveBlobAs).toHaveBeenCalledTimes(1);
		const [blob, name] = host.saveBlobAs.mock.calls[0] as [Blob, string];
		expect(name).toBe('report.pdf');
		expect(blob.size).toBe(5);
		expect(blob.type).toBe('application/pdf');
	});
});
