import { beforeEach, describe, expect, it, vi } from 'vitest';
import { dismissToast, useToastStore } from '@/lib/toast';

const shims = vi.hoisted(() => ({
	openExternalUrl: vi.fn(async () => {}),
	saveBlobAs: vi.fn(),
}));

vi.mock('@/lib/transport/shims', () => ({
	isExternalUrl: (u: string) => /^(https?:|mailto:)/.test(u),
	openExternalUrl: shims.openExternalUrl,
	saveBlobAs: shims.saveBlobAs,
}));

import { hostDownloadFile, hostOpenLink } from './host-mediated';

beforeEach(() => {
	shims.openExternalUrl.mockClear();
	shims.saveBlobAs.mockClear();
	while (useToastStore.getState().queue.length) dismissToast();
});

describe('hostOpenLink', () => {
	it('opens a web address', async () => {
		expect(await hostOpenLink('https://github.com/o/r/pull/1')).toEqual({});
		expect(shims.openExternalUrl).toHaveBeenCalledWith('https://github.com/o/r/pull/1');
	});

	it('refuses anything else and says so', async () => {
		expect(await hostOpenLink('/etc/passwd')).toEqual({ isError: true });
		expect(shims.openExternalUrl).not.toHaveBeenCalled();
		expect(useToastStore.getState().queue).toHaveLength(1);
	});
});

describe('hostDownloadFile', () => {
	it('saves embedded text', async () => {
		const res = await hostDownloadFile([
			{
				type: 'resource',
				resource: { uri: 'file:///x/export.csv', mimeType: 'text/csv', text: 'a,b' },
			},
		]);
		expect(res).toEqual({});
		expect(shims.saveBlobAs).toHaveBeenCalledOnce();
		expect((shims.saveBlobAs.mock.calls[0] as unknown[])[1]).toBe('export.csv');
	});

	it('saves an embedded base64 blob', async () => {
		const res = await hostDownloadFile([
			{
				type: 'resource',
				resource: { uri: 'x/video.mp4', mimeType: 'video/mp4', blob: btoa('abc') },
			},
		]);
		expect(res).toEqual({});
		const blob = (shims.saveBlobAs.mock.calls[0] as unknown[])[0] as Blob;
		expect(blob.size).toBe(3);
	});

	it('reports content it cannot save instead of dropping it', async () => {
		const res = await hostDownloadFile([{ type: 'resource_link', uri: 'file:///x' }]);
		expect(res).toEqual({ isError: true });
		expect(useToastStore.getState().queue).toHaveLength(1);
	});

	it('reports an empty request', async () => {
		expect(await hostDownloadFile([])).toEqual({ isError: true });
	});
});
