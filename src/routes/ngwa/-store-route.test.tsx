// /ngwa/store — R57 wiring: the signed catalog's rows reach the Store, the
// catalog auto-update sweep runs once on mount with the catalog's pins (Q3),
// and a catalog that failed to verify is shown unavailable (never the seed).

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, screen, waitFor } from '@testing-library/react';
import * as cmd from '@/lib/tauri-cmd';
import { useShellStore } from '@/lib/shell/shell-store';
import { Route as StoreRoute } from './store';
import { PROJECTS, mkSnapshot, mountRoutes } from './-ngwa-test-fixtures';

const SHA = '9c41e07a1b2c3d4e5f60718293a4b5c6d7e8f901';
const HASH = `sha256-${'ab'.repeat(32)}`;

const catalog = vi.hoisted(() => ({
	state: {} as Record<string, unknown>,
}));

vi.mock('@/lib/registry/primitives', async (orig) => ({
	...(await orig<typeof import('@/lib/registry/primitives')>()),
	usePrimitiveCatalogResult: () => catalog.state,
}));

vi.mock('@/lib/registry/use-registry', async (orig) => ({
	...(await orig<typeof import('@/lib/registry/use-registry')>()),
	useRegistryIndex: () => ({
		data: { index: { pkgs: [] }, indexUrl: 'https://registry.test/index.json' },
		isLoading: false,
		error: null,
	}),
}));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	ngwaSnapshot: vi.fn(),
	claudeStoreList: vi.fn(),
	obaAutoUpdateAll: vi.fn(),
}));

const m = vi.mocked(cmd);
vi.setConfig({ testTimeout: 20_000 });

const ENTRIES = [
	{
		kind: 'skill',
		name: 'scrollytelling',
		version: '0.1.0',
		description: 'Scroll-driven storytelling',
		source: 'npx',
		url: 'royalti-io/scrollytelling',
		ref: SHA,
		hash: HASH,
	},
	{
		kind: 'hook',
		name: 'subagent-model-floor',
		version: '0.1.0',
		description: null,
		source: 'git',
		url: 'https://github.com/royalti-io/claude-hooks',
	},
];

beforeEach(() => {
	catalog.state = {
		data: { entries: ENTRIES, verified: true },
		isLoading: false,
		isSuccess: true,
		error: null,
		refetch: vi.fn(),
	};
	m.ngwaSnapshot.mockResolvedValue(mkSnapshot([]));
	m.claudeStoreList.mockResolvedValue([]);
	m.obaAutoUpdateAll.mockResolvedValue({ updated: [], current: [], errored: [] });
	useShellStore.setState({ projects: PROJECTS, activeProjectId: 'p1' } as never);
});
afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('/ngwa/store — R57', () => {
	it('lists the catalog and sweeps catalog auto-updates once, with the pins', async () => {
		mountRoutes([{ route: StoreRoute, path: '/ngwa/store' }], '/ngwa/store');
		await waitFor(() =>
			expect(document.querySelector('.srow[data-id="cat:skill:scrollytelling"]')).not.toBeNull()
		);
		expect(document.querySelector('.srow[data-id="cat:hook:subagent-model-floor"]')).not.toBeNull();
		expect(screen.getByText(/index \+ catalog signed/)).toBeDefined();
		await waitFor(() => expect(m.obaAutoUpdateAll).toHaveBeenCalledTimes(1));
		expect(m.obaAutoUpdateAll).toHaveBeenCalledWith([
			{ kind: 'skill', name: 'scrollytelling', sha: SHA, hash: HASH },
		]);
		expect(screen.getByRole('button', { name: /Add from URL/ })).toBeDefined();
	});

	it('a catalog that failed to verify is unavailable, and no sweep runs', async () => {
		catalog.state = {
			data: undefined,
			isLoading: false,
			isSuccess: false,
			error: new Error('primitives.json signature did not verify'),
			refetch: vi.fn(),
		};
		mountRoutes([{ route: StoreRoute, path: '/ngwa/store' }], '/ngwa/store');
		await waitFor(() =>
			expect(document.querySelector('[data-catalog-unavailable]')).not.toBeNull()
		);
		expect(document.querySelector('[data-catalog-row]')).toBeNull();
		expect(m.obaAutoUpdateAll).not.toHaveBeenCalled();
	});

	it('?addurl opens the Add from URL sheet', async () => {
		mountRoutes([{ route: StoreRoute, path: '/ngwa/store' }], '/ngwa/store?addurl=1');
		await waitFor(() => expect(document.querySelector('[data-addurl-sheet]')).not.toBeNull());
	});
});
