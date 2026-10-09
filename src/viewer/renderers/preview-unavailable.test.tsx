// Gap audit rank 8 — HTML / audio / video panes in a browser session mount the
// daemon's own `/__viewer/<token>/` route (same origin), never the browser's
// own localhost, and stop that mount when the pane unmounts or switches file
// (founder decision: in-app previews only, no URL outlives its pane).

import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

const qc = new QueryClient();
const renderWithProviders = (el: React.ReactElement) =>
	render(<QueryClientProvider client={qc}>{el}</QueryClientProvider>);

const h = vi.hoisted(() => ({
	remote: false,
	viewerServe: vi.fn(async () => ({ token: 't', url: '/v/t/' })),
	viewerPort: vi.fn(async () => 4000),
	viewerStop: vi.fn(async () => {}),
	fsRead: vi.fn(async () => ({ bytes: new Uint8Array() })),
}));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	isRemoteWebSession: () => h.remote,
	viewerServe: h.viewerServe,
	viewerPort: h.viewerPort,
	viewerStop: h.viewerStop,
	fsRead: h.fsRead,
}));

import { resolveHtmlViewerUrl } from '../lib/viewer-url';
import { AudioView } from './audio-view';
import { HtmlFrame } from './html-frame';
import { VideoView } from './video-view';

afterEach(() => {
	cleanup();
	h.remote = false;
	vi.clearAllMocks();
});

describe.each([
	['HtmlFrame', () => <HtmlFrame path="/p/a.html" />],
	['AudioView', () => <AudioView path="/p/a.mp3" />],
	['VideoView', () => <VideoView path="/p/a.mp4" />],
])('%s in a remote session', (_name, el) => {
	it('mounts the viewer and renders preview element', async () => {
		h.remote = true;
		const { container } = renderWithProviders(el());
		await vi.waitFor(() => {
			expect(container.querySelector('iframe, audio, video')).not.toBeNull();
		});
		expect(h.viewerServe).toHaveBeenCalled();
		// Same-origin daemon route, never the browser's own localhost.
		const node = container.querySelector('iframe, audio, video') as HTMLElement;
		const src = node.getAttribute('src') ?? '';
		expect(src).not.toMatch(/localhost:4000|127\.0\.0\.1/);
		expect(src).toMatch(/\/v\/t\/a\.(html|mp3|mp4)$/);
		expect(h.viewerPort).not.toHaveBeenCalled();
		// The HTML frame stays sandboxed WITHOUT allow-same-origin (the page must
		// not act with the session cookie).
		if (node.tagName === 'IFRAME') {
			expect(node.getAttribute('sandbox')).toBe('allow-scripts');
		}
	});
});

describe.each([
	['HtmlFrame', (path: string) => <HtmlFrame path={path} />, 'a.html', 'b.html'],
	['AudioView', (path: string) => <AudioView path={path} />, 'a.mp3', 'b.mp3'],
	['VideoView', (path: string) => <VideoView path={path} />, 'a.mp4', 'b.mp4'],
])('%s viewer mount lifetime (remote)', (_name, el, first, second) => {
	it('calls viewer_stop on unmount', async () => {
		h.remote = true;
		h.viewerServe.mockResolvedValueOnce({ token: 'tok-1', url: '/v/tok-1/' });
		const { container, unmount } = renderWithProviders(el(`/p/${first}`));
		await vi.waitFor(() => {
			expect(container.querySelector('iframe, audio, video')).not.toBeNull();
		});
		expect(h.viewerStop).not.toHaveBeenCalled();
		unmount();
		expect(h.viewerStop).toHaveBeenCalledWith('tok-1');
	});

	it('calls viewer_stop for the old mount when the file switches', async () => {
		h.remote = true;
		h.viewerServe.mockResolvedValueOnce({ token: 'tok-1', url: '/v/tok-1/' });
		h.viewerServe.mockResolvedValueOnce({ token: 'tok-2', url: '/v/tok-2/' });
		const ui = (path: string) => <QueryClientProvider client={qc}>{el(path)}</QueryClientProvider>;
		const { container, rerender, unmount } = render(ui(`/p/${first}`));
		await vi.waitFor(() => {
			expect(container.querySelector('[src*="tok-1"]')).not.toBeNull();
		});
		rerender(ui(`/p/${second}`));
		await vi.waitFor(() => {
			expect(container.querySelector('[src*="tok-2"]')).not.toBeNull();
		});
		expect(h.viewerStop).toHaveBeenCalledWith('tok-1');
		expect(h.viewerStop).not.toHaveBeenCalledWith('tok-2');
		unmount();
		expect(h.viewerStop).toHaveBeenCalledWith('tok-2');
	});

	it('stops a mount that resolves after the pane already unmounted', async () => {
		h.remote = true;
		let release: (v: { token: string; url: string }) => void = () => {};
		h.viewerServe.mockImplementationOnce(
			() => new Promise((r) => { release = r; })
		);
		const { unmount } = renderWithProviders(el(`/p/${first}`));
		// HtmlFrame reads the file first; unmount only once the mount request is in flight.
		await vi.waitFor(() => expect(h.viewerServe).toHaveBeenCalled());
		unmount();
		release({ token: 'late', url: '/v/late/' });
		await vi.waitFor(() => expect(h.viewerStop).toHaveBeenCalledWith('late'));
	});
});

describe('resolveHtmlViewerUrl', () => {
	it('refuses in a remote session and registers no mount (in-app previews only)', async () => {
		h.remote = true;
		await expect(resolveHtmlViewerUrl('/p/a.html')).rejects.toThrow(/Not available on this server/);
		expect(h.viewerServe).not.toHaveBeenCalled();
	});

	it('builds the localhost URL on the desktop', async () => {
		const url = await resolveHtmlViewerUrl('/p/a.html');
		expect(url).toBe('http://localhost:4000/v/t/a.html');
	});
});
