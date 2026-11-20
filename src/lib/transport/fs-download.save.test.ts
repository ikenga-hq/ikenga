import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	browser: true,
	cookie: true,
	share: null as { projectKey: string } | null,
	openLocalPath: vi.fn(async (_p: string, _o?: unknown) => {}),
	downloadServerFile: vi.fn(async (_p: string) => {}),
	saveBlobAs: vi.fn((_b: Blob, _n: string) => {}),
	token: null as string | null,
}));

vi.mock('./index', () => ({
	isBrowserHost: () => h.browser,
	currentShare: () => h.share,
	transportToken: () => h.token,
}));
vi.mock('./t1-session', () => ({ isT1Session: () => h.cookie }));
vi.mock('./device-session', () => ({ isDeviceSession: () => false }));
vi.mock('./shims', () => ({
	openLocalPath: h.openLocalPath,
	downloadServerFile: h.downloadServerFile,
	saveBlobAs: h.saveBlobAs,
}));

import { downloadFile, fsDownloadUrl } from './fs-download';

let clicked: HTMLAnchorElement[] = [];

beforeEach(() => {
	h.browser = true;
	h.cookie = true;
	h.share = null;
	h.token = null;
	h.saveBlobAs.mockClear();
	h.openLocalPath.mockClear();
	h.downloadServerFile.mockClear();
	clicked = [];
	vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (
		this: HTMLAnchorElement
	) {
		clicked.push(this);
	});
});
afterEach(() => {
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

describe('fsDownloadUrl share', () => {
	it('adds ?share= only when given', () => {
		expect(fsDownloadUrl('/a.png')).not.toContain('share');
		const u = new URL(fsDownloadUrl('/a.png', { share: 'o/p' }), 'http://x');
		expect(u.searchParams.get('share')).toBe('o/p');
	});
});

describe('downloadFile', () => {
	it('desktop keeps the OS opener', async () => {
		h.browser = false;
		await downloadFile('/home/u/a.zip');
		expect(h.openLocalPath).toHaveBeenCalledWith('/home/u/a.zip', { kind: 'file' });
		expect(clicked).toHaveLength(0);
	});

	it('cookie session: navigates to the streamed route', async () => {
		await downloadFile('/srv/a b.png');
		expect(clicked).toHaveLength(1);
		const u = new URL(clicked[0].href, 'http://x');
		expect(u.pathname).toBe('/api/fs/download');
		expect(u.searchParams.get('path')).toBe('/srv/a b.png');
		expect(clicked[0].download).toBe('a b.png');
		expect(h.downloadServerFile).not.toHaveBeenCalled();
	});

	it('inside a share the share is selected on the URL', async () => {
		h.share = { projectKey: '00000000-0000-0000-0000-000000000000/proj' };
		await downloadFile('/srv/a.png');
		const u = new URL(clicked[0].href, 'http://x');
		expect(u.searchParams.get('share')).toBe('00000000-0000-0000-0000-000000000000/proj');
	});

	it('bearer-token tab: fetches the streamed route with the header and saves the bytes', async () => {
		h.cookie = false;
		h.token = 'tok';
		const fetchMock = vi.fn(async (_u: string, _i?: RequestInit) => new Response('hello'));
		vi.stubGlobal('fetch', fetchMock);
		await downloadFile('/srv/big file.bin');
		const [url, init] = fetchMock.mock.calls[0];
		expect(new URL(url, 'http://x').pathname).toBe('/api/fs/download');
		expect((init?.headers as Record<string, string>).Authorization).toBe('Bearer tok');
		expect(h.saveBlobAs).toHaveBeenCalledTimes(1);
		expect(h.saveBlobAs.mock.calls[0][0].size).toBe(5);
		expect(h.saveBlobAs.mock.calls[0][1]).toBe('big file.bin');
		// Never the capped preview read.
		expect(h.downloadServerFile).not.toHaveBeenCalled();
		expect(clicked).toHaveLength(0);
	});

	it('bearer-token tab: a refusal rejects with the server sentence', async () => {
		h.cookie = false;
		h.token = 'tok';
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => new Response(JSON.stringify({ error: 'no such file' }), { status: 404 }))
		);
		await expect(downloadFile('/srv/gone.bin')).rejects.toThrow('no such file');
		expect(h.saveBlobAs).not.toHaveBeenCalled();
	});

	it('no credential at all: the old blob path', async () => {
		h.cookie = false;
		await downloadFile('/srv/a.png');
		expect(h.downloadServerFile).toHaveBeenCalledWith('/srv/a.png');
	});
});
