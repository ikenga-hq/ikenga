// Render tests for <PkgsSurface />. The surface is mostly composition —
// titlebar, trust banner, catalog, loupe, install sheet — but the
// non-trivial bits worth pinning are:
//
//   - initialFilter prop seeds which rows render
//   - re-syncing when the prop changes (sidebar click while already
//     on /packages)
//   - the search input in the titlebar filters across the visible rows
//   - clicking trust-banner Review URL-syncs via navigate({ search })
//
// usePkgsDerived, useUpdater, useShellVersion, useNavigate, and the
// install/loupe sheets are mocked at module level so the surface mounts
// without QueryClient + a real router.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render as rtlRender, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactElement } from 'react';

import type { DerivedPkgs, PkgRowV2 } from '@/lib/pkgs/use-derived';

/** Wraps render with a fresh QueryClient — PkgRow → PkgThumb → useScreenshotSrc
 *  calls useQuery, which needs a provider even when no data is fetched. */
function render(ui: ReactElement) {
	const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return rtlRender(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

/* ───── Module mocks ───── */

const usePkgsDerivedMock = vi.fn();
vi.mock('@/lib/pkgs/use-derived', async (orig) => ({
	...(await orig<typeof import('@/lib/pkgs/use-derived')>()),
	usePkgsDerived: () => usePkgsDerivedMock(),
}));

const navigateMock = vi.fn();
vi.mock('@tanstack/react-router', async (orig) => ({
	...(await orig<typeof import('@tanstack/react-router')>()),
	useNavigate: () => navigateMock,
}));

// Loupe + install sheet are tested standalone — stub them here so the
// surface render stays focused on its own composition logic.
vi.mock('./pkg-loupe', () => ({
	PkgLoupe: ({ open }: { open: boolean }) => (open ? <div data-testid="pkg-loupe" /> : null),
}));
vi.mock('./pkg-install-sheet', () => ({
	PkgInstallSheet: ({ open }: { open: boolean }) =>
		open ? <div data-testid="pkg-install-sheet" /> : null,
}));

// Import AFTER vi.mock declarations.
import { PkgsSurface } from './pkgs-surface';

afterEach(cleanup);

/* ───── Fixtures ───── */

function makeRow(overrides: Partial<PkgRowV2> = {}): PkgRowV2 {
	return {
		id: 'com.test.x',
		name: 'TestX',
		version: '0.1.0',
		origin: 'user',
		kind: 'ui',
		state: 'idle',
		enabled: true,
		desc: '',
		installPath: '~/.config/ikenga/pkgs/com.test.x',
		installedAt: 1_700_000_000,
		latest: null,
		scopes: [],
		routes: [],
		sidecars: [],
		trust: null,
		violations: [],
		screenshots: [],
		installed: null,
		manifest: null,
		registryEntry: null,
		...overrides,
	};
}

function makeDerived(over: Partial<DerivedPkgs> = {}): DerivedPkgs {
	return {
		rows: [],
		installed: [],
		registry: [],
		updates: [],
		trust: [],
		violations: [],
		builtin: [],
		engine: [],
		user: [],
		sidecarsRunning: 0,
		isLoading: false,
		error: null,
		...over,
	};
}

const ROW_BUILTIN = makeRow({ id: 'com.ikenga.iyke', name: 'Iyke', origin: 'builtin' });
const ROW_USER = makeRow({ id: 'com.royalti.studio', name: 'Studio', origin: 'user' });
const ROW_REGISTRY = makeRow({
	id: '@ikenga/mcp-browser',
	name: 'Browser MCP',
	origin: 'registry',
	state: 'not-installed',
});
const ROW_UPDATE = makeRow({
	id: 'com.royalti.video-studio',
	name: 'Video Studio',
	version: '0.1.0',
	latest: '0.2.0',
});

beforeEach(() => {
	usePkgsDerivedMock.mockReturnValue(
		makeDerived({
			rows: [ROW_BUILTIN, ROW_USER, ROW_REGISTRY, ROW_UPDATE],
			installed: [ROW_BUILTIN, ROW_USER, ROW_UPDATE],
			registry: [ROW_REGISTRY],
			updates: [ROW_UPDATE],
			builtin: [ROW_BUILTIN],
			user: [ROW_USER, ROW_UPDATE],
			sidecarsRunning: 0,
		})
	);
	navigateMock.mockClear();
});

/* ───── Tests ───── */

describe('PkgsSurface — filter prop drives visible rows', () => {
	it('filter=all shows rows from every origin group', () => {
		render(<PkgsSurface initialFilter="all" />);
		expect(screen.getByText('Iyke')).toBeTruthy();
		expect(screen.getByText('Studio')).toBeTruthy();
		expect(screen.getByText('Browser MCP')).toBeTruthy();
		expect(screen.getByText('Video Studio')).toBeTruthy();
	});

	it('filter=installed hides registry rows', () => {
		render(<PkgsSurface initialFilter="installed" />);
		expect(screen.getByText('Iyke')).toBeTruthy();
		expect(screen.getByText('Studio')).toBeTruthy();
		expect(screen.getByText('Video Studio')).toBeTruthy();
		expect(screen.queryByText('Browser MCP')).toBeNull();
	});

	it('filter=store shows only registry rows', () => {
		render(<PkgsSurface initialFilter="store" />);
		expect(screen.getByText('Browser MCP')).toBeTruthy();
		expect(screen.queryByText('Iyke')).toBeNull();
		expect(screen.queryByText('Studio')).toBeNull();
	});

	it('filter=updates shows only outdated rows', () => {
		render(<PkgsSurface initialFilter="updates" />);
		expect(screen.getByText('Video Studio')).toBeTruthy();
		expect(screen.queryByText('Iyke')).toBeNull();
	});

	it('filter=disabled shows only disabled installed rows', () => {
		const DISABLED = makeRow({
			id: 'com.royalti.hyperframes',
			name: 'Hyperframes',
			enabled: false,
		});
		usePkgsDerivedMock.mockReturnValueOnce(
			makeDerived({
				rows: [ROW_BUILTIN, DISABLED],
				installed: [ROW_BUILTIN, DISABLED],
				builtin: [ROW_BUILTIN],
				user: [DISABLED],
			})
		);
		render(<PkgsSurface initialFilter="disabled" />);
		expect(screen.getByText('Hyperframes')).toBeTruthy();
		expect(screen.queryByText('Iyke')).toBeNull();
	});

	it('filter=review shows trust-pending + violations, deduplicated', () => {
		const REVIEW_ROW = makeRow({
			id: 'com.royalti.storyboard',
			name: 'Storyboard',
			trust: {
				pkg_id: 'com.royalti.storyboard',
				version: '0.1.0',
				state: 'needs_approval',
				perms: { shell_execute: [], fs_write_outside_sandbox: [], net: [], vault_keys: [] },
				last_granted_at_ms: null,
				change_reason: null,
				auto_trusted: false,
			},
		});
		usePkgsDerivedMock.mockReturnValueOnce(
			makeDerived({
				rows: [ROW_BUILTIN, REVIEW_ROW],
				installed: [ROW_BUILTIN, REVIEW_ROW],
				trust: [REVIEW_ROW],
				violations: [REVIEW_ROW],
				builtin: [ROW_BUILTIN],
				user: [REVIEW_ROW],
			})
		);
		render(<PkgsSurface initialFilter="review" />);
		// Storyboard appears exactly once (trust + violations dedup).
		expect(screen.getAllByText('Storyboard')).toHaveLength(1);
		expect(screen.queryByText('Iyke')).toBeNull();
	});
});

describe('PkgsSurface — initialFilter prop re-sync', () => {
	it('re-applies when the prop flips mid-mount (sidebar click while on /packages)', () => {
		// Hand-roll the wrapper so rerender keeps the QueryClient in scope.
		const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
		const { rerender } = rtlRender(
			<QueryClientProvider client={client}>
				<PkgsSurface initialFilter="installed" />
			</QueryClientProvider>
		);
		expect(screen.getByText('Iyke')).toBeTruthy();
		expect(screen.queryByText('Browser MCP')).toBeNull();
		rerender(
			<QueryClientProvider client={client}>
				<PkgsSurface initialFilter="store" />
			</QueryClientProvider>
		);
		expect(screen.queryByText('Iyke')).toBeNull();
		expect(screen.getByText('Browser MCP')).toBeTruthy();
	});
});

describe('PkgsSurface — search input', () => {
	it('filters visible rows by name', async () => {
		const user = userEvent.setup();
		render(<PkgsSurface initialFilter="all" />);
		const search = screen.getByPlaceholderText(/Filter by name/i) as HTMLInputElement;
		await user.type(search, 'studio');
		// "Studio" + "Video Studio" both match.
		expect(screen.getByText('Studio')).toBeTruthy();
		expect(screen.getByText('Video Studio')).toBeTruthy();
		// Iyke + Browser MCP filtered out.
		expect(screen.queryByText('Iyke')).toBeNull();
		expect(screen.queryByText('Browser MCP')).toBeNull();
	});

	it('shows the empty-message when nothing matches', async () => {
		const user = userEvent.setup();
		render(<PkgsSurface initialFilter="all" />);
		const search = screen.getByPlaceholderText(/Filter by name/i);
		await user.type(search, 'nonexistent-zzz');
		expect(screen.getByText(/No packages match "nonexistent-zzz"/i)).toBeTruthy();
	});

	it('filters by id substring', async () => {
		const user = userEvent.setup();
		render(<PkgsSurface initialFilter="all" />);
		const search = screen.getByPlaceholderText(/Filter by name/i);
		await user.type(search, '@ikenga');
		expect(screen.getByText('Browser MCP')).toBeTruthy();
		expect(screen.queryByText('Iyke')).toBeNull();
	});
});

describe('PkgsSurface — wiring', () => {
	it('clicking [Install pkg] opens the install sheet', async () => {
		const user = userEvent.setup();
		render(<PkgsSurface initialFilter="all" />);
		expect(screen.queryByTestId('pkg-install-sheet')).toBeNull();
		await user.click(screen.getByRole('button', { name: /Install pkg/i }));
		expect(screen.getByTestId('pkg-install-sheet')).toBeTruthy();
	});

	it('opens the install sheet on mount when initialInstallTab is set', () => {
		render(<PkgsSurface initialInstallTab="local-path" />);
		expect(screen.getByTestId('pkg-install-sheet')).toBeTruthy();
		// And clears the search param via navigate(replace).
		expect(navigateMock).toHaveBeenCalledWith(
			expect.objectContaining({ to: '/packages', replace: true })
		);
	});

	it('trust banner Review navigates with ?filter=review', async () => {
		const user = userEvent.setup();
		const TRUST = makeRow({
			id: 'com.royalti.storyboard',
			name: 'Storyboard',
			trust: {
				pkg_id: 'com.royalti.storyboard',
				version: '0.1.0',
				state: 'needs_approval',
				perms: { shell_execute: [], fs_write_outside_sandbox: [], net: [], vault_keys: [] },
				last_granted_at_ms: null,
				change_reason: null,
				auto_trusted: false,
			},
		});
		usePkgsDerivedMock.mockReturnValueOnce(
			makeDerived({
				rows: [TRUST],
				installed: [TRUST],
				trust: [TRUST],
				user: [TRUST],
			})
		);
		render(<PkgsSurface initialFilter="all" />);
		// Banner appears because d.trust.length > 0.
		const banner = screen.getByText(/pkg needs trust review/i);
		expect(banner).toBeTruthy();
		// Two Review buttons render: the banner's "Review →" and the row's "Review".
		const bannerReview = screen.getByRole('button', { name: /Review →/i });
		await user.click(bannerReview);
		expect(navigateMock).toHaveBeenCalledWith(
			expect.objectContaining({
				to: '/packages',
				search: { filter: 'review' },
			})
		);
	});

	it('surfaces the error from the derived shape', () => {
		usePkgsDerivedMock.mockReturnValueOnce(
			makeDerived({ error: 'kernel offline', isLoading: false })
		);
		render(<PkgsSurface initialFilter="all" />);
		expect(screen.getByText(/kernel offline/i)).toBeTruthy();
	});

	it('shows a loading hint while installed list is empty + isLoading', () => {
		usePkgsDerivedMock.mockReturnValueOnce(makeDerived({ isLoading: true }));
		render(<PkgsSurface initialFilter="all" />);
		expect(screen.getByText(/Loading kernel status…/i)).toBeTruthy();
	});
});
