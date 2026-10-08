// Gap audit rank 8 — HTML / audio / video panes need the desktop's localhost
// viewer server. In a browser session each shows an explicit state, never an
// iframe/media element aimed at the browser's own localhost, and never asks the
// daemon for a viewer mount.

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
	});
});

describe('resolveHtmlViewerUrl', () => {
	it('resolves remote viewer URL against current window origin', async () => {
		h.remote = true;
		const url = await resolveHtmlViewerUrl('/p/a.html');
		expect(url).toBe(`${window.location.origin}/v/t/a.html`);
		expect(h.viewerServe).toHaveBeenCalled();
	});
});
