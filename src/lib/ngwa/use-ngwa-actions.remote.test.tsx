// Installed-surface Update in a browser session. A registry PACKAGE cannot be
// updated from one (the pkg set is the server operator's), so its actionable
// "Update to <version>" row is disabled with that reason before the click — and
// a failure that still slips through reads as the honest line, not the daemon's
// raw "not implemented". An Ọba primitive (a git / npx install in the account's
// own vault) updates through `oba_update`, which the daemon serves: its row is
// NOT gated.

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

import { PACKAGES_OPERATOR_REASON } from '@/lib/desktop-only';
import { obaCheckUpdateQueryOptions } from '@/lib/queries/claude-config';
import { NOT_AVAILABLE_ON_SERVER_YET } from '@/lib/transport/unavailable';
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
		expect(upd.disabledReason).toBe(PACKAGES_OPERATOR_REASON);
		expect(upd.disabledReason).toBe('Packages are installed by the server operator');
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
		expect(result.current.status?.text).toContain(NOT_AVAILABLE_ON_SERVER_YET);
		expect(result.current.status?.text).not.toMatch(/not implemented/i);
	});
});

describe('Installed Update — an Ọba primitive is not gated', () => {
	const skill: NgwaItem = mkItem({
		id: 'skill:pdf',
		kind: 'skill',
		name: 'pdf',
		version: 'aaaaaaa',
		origin: {
			source: 'git',
			url: 'https://github.com/o/pdf',
			ref: null,
			resolved_version: 'aaaaaaa1111111',
			publisher: null,
			managed: true,
			auto_update: false,
			installed_at_ms: 1,
			updated_at_ms: 1,
		},
	});

	function setupSkill() {
		const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
		// The remote check the detail would have run: the remote moved on.
		qc.setQueryData(obaCheckUpdateQueryOptions('skill', 'pdf').queryKey, {
			current: 'aaaaaaa1111111',
			latest: 'bbbbbbb2222222',
			behind: true,
		});
		const wrapper = ({ children }: { children: ReactNode }) => (
			<QueryClientProvider client={qc}>{children}</QueryClientProvider>
		);
		return renderHook(
			() => useNgwaItemActions({ items: [skill], storeCatalog: [], unreadableSources: [] }),
			{ wrapper }
		);
	}

	it('offers "Update to <sha>" enabled in a browser session', () => {
		remote.on = true;
		const { result } = setupSkill();
		const upd = result.current.actionsFor(skill).update;
		expect(upd.label).toBe('Update to bbbbbbb');
		expect(upd.disabledReason).toBeUndefined();
	});

	it('is the same on the desktop', () => {
		remote.on = false;
		const { result } = setupSkill();
		expect(result.current.actionsFor(skill).update.disabledReason).toBeUndefined();
	});
});
