// Installed-surface Update in a browser session (gap audit rank 3): the daemon
// serves no oba_update / install, so an actionable "Update to <version>" row is
// disabled with the honest reason before the click — and a failure that still
// slips through reads as that reason, not the daemon's raw "not implemented".

import type { NgwaItem } from '@ikenga/contract';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, renderHook, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { mkItem } from '@/routes/ngwa/-ngwa-test-fixtures';
import type { NgwaStoreEntry } from './enrichment';

const remote = vi.hoisted(() => ({ on: true }));
const pkgInstallFromRegistryMock = vi.hoisted(() => vi.fn());

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	isRemoteWebSession: () => remote.on,
	pkgInstallFromRegistry: (...a: unknown[]) => pkgInstallFromRegistryMock(...a),
	pkgTrustPreviewIncoming: vi.fn().mockResolvedValue(null),
}));

vi.mock('@/lib/registry/client', async (orig) => ({
	...(await orig<typeof import('@/lib/registry/client')>()),
	fetchPkgDetail: vi.fn().mockResolvedValue({ name: '@ikenga/pkg-studio', versions: [] }),
	resolveInstallPlan: vi.fn().mockResolvedValue([
		{
			name: '@ikenga/pkg-studio',
			version: '0.8.0',
			pkgId: 'com.ikenga.studio',
			tarball: 'https://t/s.tgz',
			integrity: 'sha512-x',
		},
	]),
}));

vi.mock('@/lib/registry/use-registry', async (orig) => ({
	...(await orig<typeof import('@/lib/registry/use-registry')>()),
	useRegistryIndex: () => ({ data: { indexUrl: 'https://registry.test/index.json' } }),
}));

import { NOT_AVAILABLE_ON_SERVER } from '@/lib/transport/unavailable';
import { useNgwaItemActions } from './use-ngwa-actions';

const item: NgwaItem = mkItem({
	id: 'com.ikenga.studio',
	kind: 'app',
	name: '@ikenga/pkg-studio',
	version: '0.7.0',
});
const entry = {
	id: '@ikenga/pkg-studio',
	name: '@ikenga/pkg-studio',
	displayName: 'pkg-studio',
	description: null,
	version: '0.8.0',
	latestVersion: '0.8.0',
	kind: 'app',
	trustFacet: 'signed',
	installedItem: item,
	isUpdate: true,
	registryEntry: { name: '@ikenga/pkg-studio', latest: '0.8.0', detail: 'pkgs/studio.json' },
} as unknown as NgwaStoreEntry;

function setup() {
	const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	const wrapper = ({ children }: { children: ReactNode }) => (
		<QueryClientProvider client={qc}>{children}</QueryClientProvider>
	);
	return renderHook(
		() => useNgwaItemActions({ items: [item], storeCatalog: [entry], unreadableSources: [] }),
		{ wrapper }
	);
}

beforeEach(() => {
	pkgInstallFromRegistryMock.mockReset();
});

describe('Installed Update — install gate', () => {
	it('disables "Update to <version>" with the honest reason in a browser session', () => {
		remote.on = true;
		const { result } = setup();
		const upd = result.current.actionsFor(item).update;
		expect(upd.label).toBe('Update to 0.8.0');
		expect(upd.disabledReason).toBe(NOT_AVAILABLE_ON_SERVER);
	});

	it('leaves Update enabled on the desktop', () => {
		remote.on = false;
		const { result } = setup();
		expect(result.current.actionsFor(item).update.disabledReason).toBeUndefined();
	});

	it("never shows the daemon's raw 'not implemented' string if a click gets through", async () => {
		remote.on = false; // gate off so the click runs; the failure path is what's tested
		pkgInstallFromRegistryMock.mockRejectedValue(
			new Error("Command 'pkg_install_from_registry' not implemented in headless daemon")
		);
		const { result } = setup();
		await act(async () => {
			result.current.actionsFor(item).update.run();
		});
		await waitFor(() => expect(result.current.status?.tone).toBe('err'));
		expect(result.current.status?.text).toContain(NOT_AVAILABLE_ON_SERVER);
		expect(result.current.status?.text).not.toMatch(/not implemented/i);
	});
});
