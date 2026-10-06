// Gap audit rank 15 — Settings pages showed desktop-only errors in a browser:
// Screenshots sat on "Loading…" (screenshot_get_config unserved), the iyke MCP
// card blamed "a normal install" (iyke_mcp_info unserved), and the pane
// Screenshot item failed (screenshot_pane unserved). A remote session now
// shows "Desktop app only" and never sends those commands; the desktop still
// does.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	remote: false,
	screenshotGetConfig: vi.fn(async () => ({
		effectiveDir: '/home/u/Pictures/ikenga',
		defaultDir: '/home/u/Pictures/ikenga',
		overrideDir: null,
	})),
	screenshotSetDir: vi.fn(async () => {}),
	iykeMcpInfo: vi.fn(async () => ({
		path: '/opt/ikenga/mcp-iyke',
		present: true,
		source: 'bundled',
	})),
}));

vi.mock('@/lib/tauri-cmd', () => ({
	isRemoteWebSession: () => h.remote,
	screenshotGetConfig: h.screenshotGetConfig,
	screenshotSetDir: h.screenshotSetDir,
	iykeMcpInfo: h.iykeMcpInfo,
}));

import { desktopOnlyReason } from '@/lib/desktop-only';
import { DESKTOP_ONLY_IYKE_MCP, IykeMcpSection } from './iyke-mcp';
import { DESKTOP_ONLY_SCREENSHOTS, ScreenshotDirSectionBody } from './screenshot-dir';

function withQuery(node: ReactNode) {
	const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return <QueryClientProvider client={qc}>{node}</QueryClientProvider>;
}

beforeEach(() => {
	h.screenshotGetConfig.mockClear();
	h.iykeMcpInfo.mockClear();
});
afterEach(() => {
	cleanup();
	h.remote = false;
});

describe('Screenshots section (gap rank 15)', () => {
	it('shows desktop-only copy and never asks for the config in a remote session', () => {
		h.remote = true;
		render(<ScreenshotDirSectionBody />);
		expect(screen.getByText(DESKTOP_ONLY_SCREENSHOTS)).toBeTruthy();
		expect(screen.queryByText('Loading…')).toBeNull();
		expect(h.screenshotGetConfig).not.toHaveBeenCalled();
	});

	it('loads the config on the desktop', async () => {
		render(<ScreenshotDirSectionBody />);
		await waitFor(() => expect(screen.getByText('/home/u/Pictures/ikenga')).toBeTruthy());
		expect(h.screenshotGetConfig).toHaveBeenCalledTimes(1);
	});
});

describe('Iyke MCP card (gap rank 15)', () => {
	it('shows desktop-only copy and never calls iyke_mcp_info in a remote session', () => {
		h.remote = true;
		render(withQuery(<IykeMcpSection />));
		expect(screen.getByText(DESKTOP_ONLY_IYKE_MCP)).toBeTruthy();
		expect(screen.queryByText(/normal install/)).toBeNull();
		expect(h.iykeMcpInfo).not.toHaveBeenCalled();
	});

	it('resolves the binary path on the desktop', async () => {
		render(withQuery(<IykeMcpSection />));
		await waitFor(() => expect(h.iykeMcpInfo).toHaveBeenCalledTimes(1));
		await waitFor(() => expect(screen.getByDisplayValue('/opt/ikenga/mcp-iyke')).toBeTruthy());
	});
});

// The pane menu's Screenshot row is disabled through `desktopOnlyReason()`
// (pane-toolbar.tsx `disabled('pane.screenshot')`).
describe('pane Screenshot item (gap rank 15)', () => {
	it('is disabled as desktop-only in a remote session', () => {
		h.remote = true;
		expect(desktopOnlyReason()).toBe('Desktop app only');
	});

	it('is enabled on the desktop', () => {
		expect(desktopOnlyReason()).toBe(false);
	});
});
