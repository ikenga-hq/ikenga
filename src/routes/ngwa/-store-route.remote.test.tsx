// /ngwa/store in a browser session. The daemon serves the Ọba git / npx
// installers (WP-18b part c), so a catalog primitive's Install is live; what it
// cannot install it names before the click — a local path in Add from URL
// reads "Local installs are desktop-only", and registry packages read
// "Packages are installed by the server operator" (covered where a package
// row exists: ngwa-store-primitives.test / use-ngwa-actions.remote.test).

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, screen, waitFor } from '@testing-library/react';
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
	it('leaves a catalog primitive installable in a browser session', async () => {
		remote.on = true;
		const install = await selectCatalogRow();
		expect(install.disabled).toBe(false);
		expect(install.title).not.toMatch(/not available/i);
	});

	it('stops a local path in Add from URL with the desktop-only reason', async () => {
		remote.on = true;
		mountRoutes([{ route: StoreRoute, path: '/ngwa/store' }], '/ngwa/store');
		await waitFor(() => expect(document.querySelector('[data-id^="cat:"]')).not.toBeNull());
		fireEvent.click(screen.getByRole('button', { name: /Add from URL/ }));
		const input = (await screen.findByLabelText('git URL or npx package')) as HTMLInputElement;
		fireEvent.change(input, { target: { value: '/home/me/skills/pdf' } });
		const resolve = screen.getByRole('button', { name: 'Resolve' }) as HTMLButtonElement;
		expect(resolve.disabled).toBe(true);
		expect(resolve.title).toBe('Local installs are desktop-only');
		fireEvent.change(input, { target: { value: 'https://github.com/o/pdf' } });
		expect(resolve.disabled).toBe(false);
	});

	it('leaves Install enabled on the desktop', async () => {
		remote.on = false;
		const install = await selectCatalogRow();
		expect(install.disabled).toBe(false);
	});

	it('does not block a local path on the desktop', async () => {
		remote.on = false;
		mountRoutes([{ route: StoreRoute, path: '/ngwa/store' }], '/ngwa/store');
		await waitFor(() => expect(document.querySelector('[data-id^="cat:"]')).not.toBeNull());
		fireEvent.click(screen.getByRole('button', { name: /Add from URL/ }));
		const input = (await screen.findByLabelText('git URL or npx package')) as HTMLInputElement;
		fireEvent.change(input, { target: { value: '/home/me/skills/pdf' } });
		expect((screen.getByRole('button', { name: 'Resolve' }) as HTMLButtonElement).disabled).toBe(
			false
		);
	});
});
