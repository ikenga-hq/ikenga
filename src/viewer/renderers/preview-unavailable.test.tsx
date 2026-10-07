// Gap audit rank 8 — HTML / audio / video panes need the desktop's localhost
// viewer server. In a browser session each shows an explicit state, never an
// iframe/media element aimed at the browser's own localhost, and never asks the
// daemon for a viewer mount.

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

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
import { PREVIEW_UNAVAILABLE_BROWSER } from './preview-unavailable';
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
	it('shows the explicit unavailable state and starts no viewer mount', () => {
		h.remote = true;
		const { container } = render(el());
		expect(screen.getByText(PREVIEW_UNAVAILABLE_BROWSER)).toBeTruthy();
		expect(container.querySelector('iframe, audio, video')).toBeNull();
		expect(h.viewerServe).not.toHaveBeenCalled();
	});
});

describe('resolveHtmlViewerUrl', () => {
	it('refuses a browser-local URL in a remote session', async () => {
		h.remote = true;
		await expect(resolveHtmlViewerUrl('/p/a.html')).rejects.toThrow(PREVIEW_UNAVAILABLE_BROWSER);
		expect(h.viewerServe).not.toHaveBeenCalled();
	});
});
