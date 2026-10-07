// Gap audit rank 20 — a native-webview pkg route can never mount in a browser
// session: the route says "Desktop app only" instead of asking the daemon for a
// child webview it cannot create. The desktop still mounts the webview host.

import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ remote: false }));

vi.mock('@tanstack/react-router', () => ({
	createFileRoute: () => (opts: { component: unknown }) => ({
		...opts,
		useParams: () => ({ pkgId: 'com.example.web', _splat: '' }),
	}),
	useNavigate: () => vi.fn(),
}));
vi.mock('@/lib/tauri-cmd', () => ({
	isRemoteWebSession: () => h.remote,
	pkgKernelStatus: async () => ({
		registries: {
			ui_routes: {
				entries: [
					{
						pkg_id: 'com.example.web',
						virtual_path: 'pkg://com.example.web/',
						path: '/',
						kind: 'webview',
						source: 'https://example.test',
					},
				],
			},
		},
	}),
	pkgTrustApprove: vi.fn(),
	pkgTrustListPending: async () => [],
}));
vi.mock('@/components/pkg/pkg-webview-host', () => ({
	PkgWebviewHost: () => <div data-testid="webview-host" />,
}));
vi.mock('@/components/pkg/pkg-iframe-host', () => ({ PkgIframeHost: () => null }));
vi.mock('@/components/pkg/actions/action-bar', () => ({ ActionBar: () => null }));
vi.mock('@/components/pkg/pkg-view-states', () => ({ PkgConsentState: () => null }));
vi.mock('@/shell/panes/views/route-view', () => ({ usePaneScope: () => null }));

import { Route } from './$';

afterEach(() => {
	cleanup();
	h.remote = false;
});

const Page = (Route as unknown as { component: () => React.ReactNode }).component;

describe('/pkg/$pkgId webview route (gap rank 20)', () => {
	it('shows "Desktop app only" in a browser session and never mounts the webview', async () => {
		h.remote = true;
		render(<Page />);
		await waitFor(() => expect(screen.getByText('Desktop app only')).toBeTruthy());
		expect(screen.queryByTestId('webview-host')).toBeNull();
	});

	it('mounts the webview host on the desktop', async () => {
		render(<Page />);
		await waitFor(() => expect(screen.getByTestId('webview-host')).toBeTruthy());
	});
});
