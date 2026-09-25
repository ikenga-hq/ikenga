// Unit tests for the WP-41-F1 pre-install capability diff in useUpdatePkgs:
// a row whose incoming version requests a new capability/permission must be
// parked into `needsApproval` and NOT installed — `pkgInstallFromRegistry`
// must not be called for it — while a row the caller already approved
// (`approvedIds`) skips the diff and installs normally.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { renderHook } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

import type { PkgRowV2 } from './use-derived';
import type { PkgTrustReview } from '@/lib/tauri-cmd';

const pkgInstallFromRegistryMock = vi.fn().mockResolvedValue({ installed: { id: 'com.x' } });
const pkgTrustPreviewIncomingMock = vi.fn();

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	pkgInstallFromRegistry: (...args: unknown[]) => pkgInstallFromRegistryMock(...args),
	pkgTrustPreviewIncoming: (...args: unknown[]) => pkgTrustPreviewIncomingMock(...args),
}));

const fetchPkgDetailMock = vi.fn();
const resolveInstallPlanMock = vi.fn();

vi.mock('@/lib/registry/client', async (orig) => ({
	...(await orig<typeof import('@/lib/registry/client')>()),
	fetchPkgDetail: (...args: unknown[]) => fetchPkgDetailMock(...args),
	resolveInstallPlan: (...args: unknown[]) => resolveInstallPlanMock(...args),
}));

const useRegistryIndexMock = vi.fn();

vi.mock('@/lib/registry/use-registry', async (orig) => ({
	...(await orig<typeof import('@/lib/registry/use-registry')>()),
	useRegistryIndex: () => useRegistryIndexMock(),
}));

// Import AFTER the mocks are declared so the hook picks them up.
import { useUpdatePkgs } from './use-update-pkgs';

function makeRow(id: string, overrides: Partial<PkgRowV2> = {}): PkgRowV2 {
	return {
		id,
		name: id,
		version: '1.0.0',
		origin: 'registry',
		kind: 'ui',
		state: 'idle',
		enabled: true,
		desc: '',
		installPath: `/pkgs/${id}`,
		installedAt: null,
		latest: '1.1.0',
		scopes: [],
		routes: [],
		sidecars: [],
		trust: null,
		violations: [],
		screenshots: [],
		installed: null,
		manifest: null,
		registryEntry: { name: id, latest: '1.1.0' } as PkgRowV2['registryEntry'],
		...overrides,
	};
}

function wrapper({ children }: { children: ReactNode }) {
	const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return <QueryClientProvider client={qc}>{children}</QueryClientProvider>;
}

const sampleReview: PkgTrustReview = {
	pkg_id: 'com.ikenga.studio',
	manifest_version: '1.1.0',
	old_capabilities: '{"capabilities":null,"permissions":{}}',
	new_capabilities: '{"capabilities":{"http":{}},"permissions":{}}',
	prior_approved_at_ms: 0,
};

beforeEach(() => {
	useRegistryIndexMock.mockReturnValue({ data: { indexUrl: 'https://example/index.json' } });
	fetchPkgDetailMock.mockImplementation(async (_url: string, entry: { name: string }) => ({
		name: entry.name,
		version: '1.1.0',
		capabilities: null,
		permissions: {},
	}));
	resolveInstallPlanMock.mockResolvedValue([
		{ name: 'x', pkgId: 'com.ikenga.studio', tarball: 'https://t', integrity: 'sha512-x' },
	]);
});

afterEach(() => {
	vi.clearAllMocks();
});

describe('useUpdatePkgs — WP-41-F1 pre-install capability diff', () => {
	it('parks a row that requests a new capability, without installing it', async () => {
		pkgTrustPreviewIncomingMock.mockResolvedValueOnce(sampleReview);
		const { result } = renderHook(() => useUpdatePkgs(), { wrapper });

		const row = makeRow('com.ikenga.studio');
		const res = await result.current.mutateAsync({ rows: [row] });

		expect(pkgTrustPreviewIncomingMock).toHaveBeenCalledWith(
			expect.objectContaining({ pkgId: 'com.ikenga.studio' })
		);
		expect(pkgInstallFromRegistryMock).not.toHaveBeenCalled();
		expect(res.updated).toBe(0);
		expect(res.needsApproval).toEqual([sampleReview]);
	});

	it('installs a row with no capability diff normally', async () => {
		pkgTrustPreviewIncomingMock.mockResolvedValueOnce(null);
		const { result } = renderHook(() => useUpdatePkgs(), { wrapper });

		const row = makeRow('com.ikenga.git');
		const res = await result.current.mutateAsync({ rows: [row] });

		expect(pkgInstallFromRegistryMock).toHaveBeenCalledTimes(1);
		expect(res.updated).toBe(1);
		expect(res.needsApproval).toEqual([]);
	});

	it('skips the diff and installs directly for a row in approvedIds', async () => {
		const { result } = renderHook(() => useUpdatePkgs(), { wrapper });

		const row = makeRow('com.ikenga.studio');
		const res = await result.current.mutateAsync({
			rows: [row],
			approvedIds: new Set(['com.ikenga.studio']),
		});

		expect(pkgTrustPreviewIncomingMock).not.toHaveBeenCalled();
		expect(pkgInstallFromRegistryMock).toHaveBeenCalledTimes(1);
		expect(res.updated).toBe(1);
		expect(res.needsApproval).toEqual([]);
	});

	it('a failing pre-install diff check records the row as failed, not parked', async () => {
		pkgTrustPreviewIncomingMock.mockRejectedValueOnce(new Error('kernel unreachable'));
		const { result } = renderHook(() => useUpdatePkgs(), { wrapper });

		const row = makeRow('com.ikenga.git');
		const res = await result.current.mutateAsync({ rows: [row] });

		expect(res.failed).toHaveLength(1);
		expect(res.failed[0]).toMatchObject({ id: 'com.ikenga.git' });
		expect(res.needsApproval).toEqual([]);
		expect(pkgInstallFromRegistryMock).not.toHaveBeenCalled();
	});
});
