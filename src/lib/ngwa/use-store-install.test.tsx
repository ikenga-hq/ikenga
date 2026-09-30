// useStoreInstall — the Ngwa Store's install/update wiring (WP-15 / D-02).
// Pins: one `pkgInstallFromRegistry` per resolved plan step, the Store →
// PkgScopeWire scope mapping, update reusing the same path in the installed
// item's own scope (after the WP-41-F1 capability diff), and the snapshot +
// registry invalidation afterwards.

import { beforeEach, describe, expect, it, vi } from 'vitest';
import { renderHook } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import type { NgwaItem } from '@ikenga/contract';
import type { NgwaStoreEntry } from './enrichment';

const pkgInstallFromRegistryMock = vi.fn().mockResolvedValue({ installed: { id: 'com.x' } });
const pkgTrustPreviewIncomingMock = vi.fn().mockResolvedValue(null);

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

vi.mock('@/lib/registry/use-registry', async (orig) => ({
	...(await orig<typeof import('@/lib/registry/use-registry')>()),
	useRegistryIndex: () => ({ data: { indexUrl: 'https://registry.test/index.json' } }),
}));

import { useShellStore } from '@/lib/shell/shell-store';
import {
	installedScopeWire,
	isNeedsApproval,
	type NeedsApprovalError,
	storeScopeWire,
	useStoreInstall,
} from './use-store-install';

const PLAN = [
	{
		name: '@ikenga/dep',
		version: '0.1.0',
		pkgId: 'com.ikenga.dep',
		tarball: 'https://t/dep.tgz',
		integrity: 'sha512-dep',
	},
	{
		name: '@ikenga/pkg-studio',
		version: '0.8.0',
		pkgId: 'com.ikenga.studio',
		tarball: 'https://t/studio.tgz',
		integrity: 'sha512-studio',
	},
];

function entry(overrides: Partial<NgwaStoreEntry> = {}): NgwaStoreEntry {
	return {
		id: '@ikenga/pkg-studio',
		name: '@ikenga/pkg-studio',
		displayName: 'pkg-studio',
		description: null,
		version: '0.8.0',
		latestVersion: '0.8.0',
		kind: 'app',
		trustFacet: 'signed',
		installedItem: null,
		isUpdate: false,
		registryEntry: { name: '@ikenga/pkg-studio', latest: '0.8.0', detail: 'pkgs/studio.json' },
		...overrides,
	};
}

function installed(scope: NgwaItem['scope']): NgwaItem {
	return { id: 'com.ikenga.studio', version: '0.7.0', scope } as unknown as NgwaItem;
}

function setup() {
	const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	const invalidate = vi.spyOn(qc, 'invalidateQueries');
	const wrapper = ({ children }: { children: ReactNode }) => (
		<QueryClientProvider client={qc}>{children}</QueryClientProvider>
	);
	const { result } = renderHook(() => useStoreInstall(), { wrapper });
	return { hook: result.current, invalidate };
}

function invalidatedKeys(spy: { mock: { calls: unknown[][] } }) {
	return spy.mock.calls.map((c) => (c[0] as { queryKey: unknown[] }).queryKey);
}

beforeEach(() => {
	pkgInstallFromRegistryMock.mockClear().mockResolvedValue({ installed: { id: 'com.x' } });
	pkgTrustPreviewIncomingMock.mockClear().mockResolvedValue(null);
	fetchPkgDetailMock.mockReset().mockResolvedValue({ name: '@ikenga/pkg-studio', versions: [] });
	resolveInstallPlanMock.mockReset().mockResolvedValue(PLAN);
	useShellStore.setState({ activeProjectId: 'royalti-co' });
});

describe('scope mapping', () => {
	it('maps personal to workspace and project to the active project', () => {
		expect(storeScopeWire('personal', 'p1')).toBe('workspace');
		expect(storeScopeWire('project', 'p1')).toBe('project:p1');
		expect(storeScopeWire('project', null)).toBeNull();
		expect(installedScopeWire({ kind: 'personal' })).toBe('workspace');
		expect(installedScopeWire({ kind: 'project', project_id: 'p2' })).toBe('project:p2');
	});
});

describe('useStoreInstall', () => {
	it('installs every plan step to the active project and invalidates the snapshot + registry', async () => {
		const { hook, invalidate } = setup();
		await hook.install(entry(), 'project');

		expect(resolveInstallPlanMock).toHaveBeenCalledTimes(1);
		expect(resolveInstallPlanMock.mock.calls[0][2]).toBe('0.8.0');
		expect(pkgInstallFromRegistryMock).toHaveBeenCalledTimes(2);
		expect(pkgInstallFromRegistryMock).toHaveBeenNthCalledWith(
			1,
			expect.objectContaining({
				pkgId: 'com.ikenga.dep',
				tarball: 'https://t/dep.tgz',
				integrity: 'sha512-dep',
				sourceUrl: 'https://t/dep.tgz',
			}),
			'project:royalti-co'
		);
		expect(pkgInstallFromRegistryMock).toHaveBeenNthCalledWith(
			2,
			expect.objectContaining({ pkgId: 'com.ikenga.studio' }),
			'project:royalti-co'
		);
		// A fresh install doesn't run the update capability diff.
		expect(pkgTrustPreviewIncomingMock).not.toHaveBeenCalled();

		const keys = invalidatedKeys(invalidate);
		expect(keys).toContainEqual(['ngwa', 'snapshot']);
		expect(keys).toContainEqual(['registry']);
		expect(keys).toContainEqual(['pkg']);
	});

	it('installs to personal as the workspace scope', async () => {
		const { hook } = setup();
		await hook.install(entry(), 'personal');
		expect(pkgInstallFromRegistryMock.mock.calls.map((c) => c[1])).toEqual([
			'workspace',
			'workspace',
		]);
	});

	it('rejects with the failing step and still invalidates', async () => {
		pkgInstallFromRegistryMock.mockRejectedValueOnce(new Error('integrity mismatch'));
		const { hook, invalidate } = setup();
		await expect(hook.install(entry(), 'project')).rejects.toThrow('integrity mismatch');
		expect(pkgInstallFromRegistryMock).toHaveBeenCalledTimes(1);
		expect(invalidatedKeys(invalidate)).toContainEqual(['ngwa', 'snapshot']);
	});

	it('updates through the same plan path, in the installed item’s own scope', async () => {
		const { hook, invalidate } = setup();
		await hook.update(
			entry({
				isUpdate: true,
				version: '0.7.0',
				installedItem: installed({ kind: 'project', project_id: 'other' }),
			})
		);

		expect(pkgTrustPreviewIncomingMock).toHaveBeenCalledWith(
			expect.objectContaining({ pkgId: 'com.ikenga.studio' })
		);
		expect(pkgInstallFromRegistryMock).toHaveBeenCalledTimes(2);
		expect(pkgInstallFromRegistryMock.mock.calls.map((c) => c[1])).toEqual([
			'project:other',
			'project:other',
		]);
		expect(invalidatedKeys(invalidate)).toContainEqual(['ngwa', 'snapshot']);
	});

	it('holds back (not fails) a pkg whose new version asks for new permissions', async () => {
		const review = { pkg_id: 'com.ikenga.studio', manifest_version: '0.8.0' };
		pkgTrustPreviewIncomingMock.mockResolvedValueOnce(review);
		const { hook } = setup();
		const e = entry({ isUpdate: true, installedItem: installed({ kind: 'personal' }) });
		const err = await hook.update(e).catch((x: unknown) => x);
		expect(isNeedsApproval(err)).toBe(true);
		expect((err as NeedsApprovalError).approvals).toEqual([{ entry: e, review }]);
		expect((err as Error).message).not.toMatch(/Installed tab/);
		expect(pkgInstallFromRegistryMock).not.toHaveBeenCalled();
	});

	it('an approved update skips the capability diff and installs', async () => {
		pkgTrustPreviewIncomingMock.mockResolvedValue({ pkg_id: 'com.ikenga.studio' });
		const { hook } = setup();
		await hook.update(entry({ isUpdate: true, installedItem: installed({ kind: 'personal' }) }), {
			approved: true,
		});
		expect(pkgTrustPreviewIncomingMock).not.toHaveBeenCalled();
		expect(pkgInstallFromRegistryMock).toHaveBeenCalledTimes(2);
	});

	it('updateAll separates approvals from failures', async () => {
		const mk = (id: string) =>
			entry({
				id,
				displayName: id,
				isUpdate: true,
				installedItem: installed({ kind: 'personal' }),
			});
		const [a, b, c] = [mk('a'), mk('b'), mk('c')];
		// a: held for approval · b: fails · c: installs.
		pkgTrustPreviewIncomingMock
			.mockResolvedValueOnce({ pkg_id: 'a' })
			.mockResolvedValueOnce(null)
			.mockResolvedValueOnce(null);
		resolveInstallPlanMock.mockRejectedValueOnce(new Error('404')).mockResolvedValueOnce([PLAN[1]]);
		const { hook } = setup();

		const err = await hook.updateAll([a, b, c]).catch((x: unknown) => x);
		expect(isNeedsApproval(err)).toBe(true);
		const held = err as NeedsApprovalError;
		expect(held.approvals.map((p) => p.entry.id)).toEqual(['a']);
		expect(held.failures).toEqual(['b: 404']);
		expect(pkgInstallFromRegistryMock).toHaveBeenCalledTimes(1);
	});

	it('updateAll runs every entry and reports the failures together', async () => {
		resolveInstallPlanMock.mockRejectedValueOnce(new Error('404')).mockResolvedValueOnce([PLAN[1]]);
		const { hook } = setup();
		const a = entry({
			id: 'a',
			displayName: 'a',
			isUpdate: true,
			installedItem: installed({ kind: 'personal' }),
		});
		const b = entry({
			id: 'b',
			displayName: 'b',
			isUpdate: true,
			installedItem: installed({ kind: 'personal' }),
		});

		await expect(hook.updateAll([a, b])).rejects.toThrow('1 of 2 updates failed — a: 404');
		expect(pkgInstallFromRegistryMock).toHaveBeenCalledTimes(1);
		expect(pkgInstallFromRegistryMock.mock.calls[0][1]).toBe('workspace');
	});
});
