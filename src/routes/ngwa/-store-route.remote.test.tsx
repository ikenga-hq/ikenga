// /ngwa/store in a browser session (gap audit rank 3): the daemon serves no
// install or update, so the sheet's Install button is disabled before the
// click, not failed after it.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, waitFor } from '@testing-library/react';
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

const remote = vi.hoisted(() => ({ on: true }));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	isRemoteWebSession: () => remote.on,
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

const selectCatalogRow = async () => {
	mountRoutes([{ route: StoreRoute, path: '/ngwa/store' }], '/ngwa/store');
	const sel = '.srow[data-id="cat:skill:scrollytelling"]';
	await waitFor(() => expect(document.querySelector(sel)).not.toBeNull());
	fireEvent.click(document.querySelector(sel) as Element);
	await waitFor(() => expect(document.querySelector('[data-install]')).not.toBeNull());
	return document.querySelector('[data-install]') as HTMLButtonElement;
};

describe('/ngwa/store — install gate', () => {
	it('disables Install before the click in a browser session', async () => {
		remote.on = true;
		const install = await selectCatalogRow();
		expect(install.disabled).toBe(true);
		expect(install.title).toMatch(/not available/i);
	});

	it('leaves Install enabled on the desktop', async () => {
		remote.on = false;
		const install = await selectCatalogRow();
		expect(install.disabled).toBe(false);
	});
});
