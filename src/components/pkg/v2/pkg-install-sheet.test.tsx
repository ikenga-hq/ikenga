// Render tests for <PkgInstallSheet />. Two modes worth pinning:
//
// 1. Generic (no pkg): three tabs (Manifest URL / Local path / Registry).
//    Manifest-URL is parked; Local path actually wires pkgInstallFromPath.
//
// 2. Pkg-targeted (registry row): screenshot preview, manifest meta,
//    write-scope callout, [Install <name>] CTA that walks the plan.
//
// We mock the registry hooks + tauri-cmd at the module level so the sheet
// can render without a real index fetch.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

import type { PkgRowV2 } from '@/lib/pkgs/use-derived';

/* ───── Module mocks ─────
   - tauri-cmd: pkgInstallFromPath + pkgInstallFromRegistry → stubs.
   - registry hooks: return controllable shapes.

   Hoisted via vi.mock so the sheet's imports resolve to these versions. */

const pkgInstallFromPathMock = vi.fn().mockResolvedValue({ installed: { id: 'com.x' } });
const pkgInstallFromRegistryMock = vi.fn().mockResolvedValue({ installed: { id: 'com.x' } });

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	pkgInstallFromPath: (...args: unknown[]) => pkgInstallFromPathMock(...args),
	pkgInstallFromRegistry: (...args: unknown[]) => pkgInstallFromRegistryMock(...args),
}));

const useRegistryIndexMock = vi.fn();
const useRegistryPkgDetailMock = vi.fn();
const useInstallPlanResolverMock = vi.fn();
const useRefreshRegistryMock = vi.fn();

vi.mock('@/lib/registry/use-registry', async (orig) => ({
	...(await orig<typeof import('@/lib/registry/use-registry')>()),
	useRegistryIndex: () => useRegistryIndexMock(),
	useRegistryPkgDetail: (...args: unknown[]) => useRegistryPkgDetailMock(...args),
	useInstallPlanResolver: (...args: unknown[]) => useInstallPlanResolverMock(...args),
	useRefreshRegistry: () => useRefreshRegistryMock(),
}));

// Import AFTER the mocks are declared so the module picks them up.
import { PkgInstallSheet } from './pkg-install-sheet';

afterEach(cleanup);

beforeEach(() => {
	useRegistryIndexMock.mockReturnValue({
		data: { indexUrl: 'https://example/index.json' },
		isLoading: false,
		error: null,
	});
	useRegistryPkgDetailMock.mockReturnValue({
		data: { versions: [{ tarball: 'https://t1', integrity: 'sha512-...' }] },
		isLoading: false,
		error: null,
	});
	useInstallPlanResolverMock.mockReturnValue({
		mutateAsync: vi.fn().mockResolvedValue([
			{
				name: '@ikenga/dep',
				pkgId: 'com.ikenga.dep',
				tarball: 'https://dep.tar',
				integrity: 'sha512-dep',
			},
			{
				name: '@ikenga/root',
				pkgId: 'com.ikenga.root',
				tarball: 'https://root.tar',
				integrity: 'sha512-root',
			},
		]),
	});
	useRefreshRegistryMock.mockReturnValue(() => {});
	pkgInstallFromPathMock.mockClear();
	pkgInstallFromRegistryMock.mockClear();
});

function withQuery(ui: ReactNode) {
	const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return <QueryClientProvider client={client}>{ui}</QueryClientProvider>;
}

function makePkg(overrides: Partial<PkgRowV2> = {}): PkgRowV2 {
	return {
		id: '@test/example',
		name: 'Example',
		version: '0.1.0',
		origin: 'registry',
		kind: 'ui',
		state: 'not-installed',
		enabled: false,
		desc: 'Does the thing.',
		installPath: '(not installed)',
		installedAt: null,
		latest: '0.1.0',
		scopes: ['fs:read:workspace', 'fs:write:.company/content'],
		routes: ['example/main'],
		sidecars: [],
		trust: null,
		violations: [],
		screenshots: [],
		installed: null,
		manifest: null,
		registryEntry: {
			name: '@test/example',
			latest: '0.1.0',
			detail: 'pkgs/example.json',
		},
		...overrides,
	};
}

/* ───── Generic mode ───── */

describe('PkgInstallSheet — generic', () => {
	it('renders three tabs and the generic header', () => {
		render(withQuery(<PkgInstallSheet open onOpenChange={() => {}} />));
		expect(screen.getByText('Install pkg')).toBeTruthy();
		expect(screen.getByRole('tab', { name: /Manifest URL/i })).toBeTruthy();
		expect(screen.getByRole('tab', { name: /Local path/i })).toBeTruthy();
		expect(screen.getByRole('tab', { name: /Registry/i })).toBeTruthy();
	});

	it('starts on the Manifest URL tab and shows the input', () => {
		render(withQuery(<PkgInstallSheet open onOpenChange={() => {}} />));
		// "manifest URL" appears on both the tab button and the section label,
		// so disambiguate via the input placeholder.
		expect(screen.getByPlaceholderText(/pkgs\.ikenga\.ai/)).toBeTruthy();
	});

	it('switching to Local path swaps the body', async () => {
		const user = userEvent.setup();
		render(withQuery(<PkgInstallSheet open onOpenChange={() => {}} />));
		await user.click(screen.getByRole('tab', { name: /Local path/i }));
		expect(screen.getByText(/absolute path/i)).toBeTruthy();
		expect(screen.getByPlaceholderText(/\/Users\/you\/my-pkg/i)).toBeTruthy();
	});

	it('defaults to a passed-in tab', () => {
		render(withQuery(<PkgInstallSheet open onOpenChange={() => {}} defaultTab="local-path" />));
		expect(screen.getByText(/absolute path/i)).toBeTruthy();
	});

	it('Local path Install fires pkgInstallFromPath with typed value', async () => {
		const user = userEvent.setup();
		render(withQuery(<PkgInstallSheet open onOpenChange={() => {}} defaultTab="local-path" />));
		const pathInput = screen.getByPlaceholderText(/\/Users\/you\/my-pkg/i);
		await user.type(pathInput, '/tmp/my-pkg');
		// The active-tab Install button (footer) is the only enabled non-Cancel.
		// Find by exact text.
		await user.click(screen.getByRole('button', { name: /^Install$/i }));
		expect(pkgInstallFromPathMock).toHaveBeenCalledTimes(1);
	});

	it('Manifest URL Install button is disabled (parked)', () => {
		render(withQuery(<PkgInstallSheet open onOpenChange={() => {}} />));
		const installBtn = screen.getByRole('button', { name: /^Install$/i });
		expect((installBtn as HTMLButtonElement).disabled).toBe(true);
	});

	it('Registry tab shows the redirect hint, no Install button', async () => {
		const user = userEvent.setup();
		render(withQuery(<PkgInstallSheet open onOpenChange={() => {}} />));
		await user.click(screen.getByRole('tab', { name: /^Registry$/i }));
		expect(screen.getByText(/registry browser opens on \/packages\?filter=store/i)).toBeTruthy();
		expect(screen.queryByRole('button', { name: /^Install$/i })).toBeNull();
	});
});

/* ───── Pkg-targeted mode ───── */

describe('PkgInstallSheet — pkg-targeted', () => {
	it('renders pkg name + version in the header', () => {
		render(withQuery(<PkgInstallSheet open onOpenChange={() => {}} pkg={makePkg()} />));
		// Header + button both render the "Install Example" string — match
		// against the unique id@version line in the header subtitle.
		expect(screen.getByText('@test/example@0.1.0')).toBeTruthy();
		// And confirm at least one "Install Example" anywhere (header + button).
		expect(screen.getAllByText(/Install Example/).length).toBeGreaterThanOrEqual(2);
	});

	it('renders the about + manifest preview sections', () => {
		render(withQuery(<PkgInstallSheet open onOpenChange={() => {}} pkg={makePkg()} />));
		expect(screen.getByText(/about/i)).toBeTruthy();
		expect(screen.getByText(/Does the thing\./)).toBeTruthy();
		expect(screen.getByText(/manifest preview · @test\/example@0\.1\.0/i)).toBeTruthy();
	});

	it('renders scope chips for each declared scope', () => {
		render(withQuery(<PkgInstallSheet open onOpenChange={() => {}} pkg={makePkg()} />));
		expect(screen.getByText('fs:read:workspace')).toBeTruthy();
		expect(screen.getByText('fs:write:.company/content')).toBeTruthy();
	});

	it('shows the write-scope callout when fs:write is declared', () => {
		render(withQuery(<PkgInstallSheet open onOpenChange={() => {}} pkg={makePkg()} />));
		expect(screen.getByText(/Review write scopes/i)).toBeTruthy();
		// `.company/content` appears in both the scope chip and the callout
		// message — confirm at least both renderings exist.
		expect(screen.getAllByText(/\.company\/content/).length).toBeGreaterThanOrEqual(2);
	});

	it('hides the write-scope callout when no fs:write scope', () => {
		render(
			withQuery(
				<PkgInstallSheet
					open
					onOpenChange={() => {}}
					pkg={makePkg({ scopes: ['fs:read:workspace'] })}
				/>
			)
		);
		expect(screen.queryByText(/Review write scopes/i)).toBeNull();
	});

	it('Install button label includes the pkg name', () => {
		render(withQuery(<PkgInstallSheet open onOpenChange={() => {}} pkg={makePkg()} />));
		expect(screen.getByRole('button', { name: /Install Example/i })).toBeTruthy();
	});

	it('clicking Install walks the plan and calls pkgInstallFromRegistry per step', async () => {
		const user = userEvent.setup();
		render(withQuery(<PkgInstallSheet open onOpenChange={() => {}} pkg={makePkg()} />));
		await user.click(screen.getByRole('button', { name: /Install Example/i }));
		// The plan had 2 steps — sheet walks both.
		expect(pkgInstallFromRegistryMock).toHaveBeenCalledTimes(2);
		expect(pkgInstallFromRegistryMock).toHaveBeenNthCalledWith(1, {
			tarball: 'https://dep.tar',
			integrity: 'sha512-dep',
			pkgId: 'com.ikenga.dep',
			sourceUrl: 'https://dep.tar',
		});
		expect(pkgInstallFromRegistryMock).toHaveBeenNthCalledWith(2, {
			tarball: 'https://root.tar',
			integrity: 'sha512-root',
			pkgId: 'com.ikenga.root',
			sourceUrl: 'https://root.tar',
		});
	});

	it('disables Install while the registry detail is still loading', () => {
		useRegistryPkgDetailMock.mockReturnValueOnce({
			data: undefined,
			isLoading: true,
			error: null,
		});
		render(withQuery(<PkgInstallSheet open onOpenChange={() => {}} pkg={makePkg()} />));
		const btn = screen.getByRole('button', { name: /Install Example/i });
		expect((btn as HTMLButtonElement).disabled).toBe(true);
		expect(screen.getByText(/resolving plan…/i)).toBeTruthy();
	});

	it('surfaces a registry-unreachable status when the index errors', () => {
		useRegistryIndexMock.mockReturnValueOnce({
			data: undefined,
			isLoading: false,
			error: new Error('offline'),
		});
		render(withQuery(<PkgInstallSheet open onOpenChange={() => {}} pkg={makePkg()} />));
		expect(screen.getByText(/registry unreachable: offline/i)).toBeTruthy();
	});
});
