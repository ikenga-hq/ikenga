// /ngwa/store in a browser session. The daemon serves the Ọba git / npx
// installers (WP-18b part c), so a catalog primitive's Install is live; what it
// cannot install it names before the click — a local path in Add from URL
// reads "Local installs are desktop-only", and registry packages read
// "Packages are installed by the server operator" (covered where a package
// row exists: ngwa-store-primitives.test / use-ngwa-actions.remote.test).
//
// A registry SKILL / BUNDLE that the signed catalog also lists ("also: npx") is
// the exception: it installs through the Ọba path into the account's vault, at
// the catalog's pin. An APP, or a skill pkg with no catalog twin, stays blocked
// in the row and the sheet alike.

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

const PKGS = [
	{ name: '@ikenga/skill-groundwork', latest: '1.0.0', kind: 'skill', description: 'Groundwork' },
	{ name: '@ikenga/skill-thunderbird', latest: '1.0.0', kind: 'skill', description: 'Mail' },
	{ name: '@ikenga/pkg-tasks', latest: '0.8.3', kind: 'app', description: 'Tasks' },
	// A "skill" pkg that ships an MCP server: the catalog lists a twin, but it is
	// really a tool, so it is not skill-only.
	{ name: '@ikenga/skill-mcpish', latest: '1.0.0', kind: 'skill', description: 'Has a server' },
];

vi.mock('@/lib/registry/use-registry', async (orig) => ({
	...(await orig<typeof import('@/lib/registry/use-registry')>()),
	useRegistryIndex: () => ({
		data: { index: { pkgs: PKGS }, indexUrl: 'https://registry.test/index.json' },
		isLoading: false,
		error: null,
	}),
}));

const detail = vi.hoisted(() => ({
	manifest: {} as Record<string, unknown>,
}));

vi.mock('@/lib/registry/client', async (orig) => ({
	...(await orig<typeof import('@/lib/registry/client')>()),
	fetchPkgVersionForStore: async (_u: string, entry: { name: string }) => ({
		version: '1.0.0',
		publishedAt: '2026-09-01T00:00:00Z',
		tarball: 'https://registry.npmjs.org/x/-/x-1.0.0.tgz',
		integrity: 'sha512-abc',
		size: 1000,
		deps: [],
		screenshots: [],
		manifest: {
			id: entry.name,
			name: entry.name,
			version: '1.0.0',
			ikenga_api: '1',
			mcp: [],
			sidecars: [],
			requires: [],
			permissions: {},
			ui: {},
			...(entry.name === '@ikenga/skill-mcpish' ? { mcp: [{ name: 'srv' }] } : {}),
			...detail.manifest,
		},
	}),
}));

const remote = vi.hoisted(() => ({ on: true }));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	isRemoteWebSession: () => remote.on,
	ngwaSnapshot: vi.fn(),
	claudeStoreList: vi.fn(),
	obaAutoUpdateAll: vi.fn(),
	obaInstallWithDeps: vi.fn(),
	claudePrimitiveEnable: vi.fn(),
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
		kind: 'skill',
		name: 'groundwork',
		version: '0.3.0',
		description: 'Plan folders',
		source: 'npx',
		url: 'royalti-io/groundwork',
		ref: SHA,
		hash: HASH,
	},
	{
		kind: 'skill',
		name: 'mcpish',
		version: '0.1.0',
		description: null,
		source: 'npx',
		url: 'royalti-io/mcpish',
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
	m.obaInstallWithDeps.mockResolvedValue({
		target: { kind: 'skill', name: 'groundwork' },
		installed: [],
		alreadySatisfied: [],
	} as never);
	m.claudePrimitiveEnable.mockResolvedValue({} as never);
	detail.manifest = {};
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

describe('/ngwa/store — registry skill / bundle packages in a browser session', () => {
	const selectRegistryRow = async (id: string) => {
		mountRoutes([{ route: StoreRoute, path: '/ngwa/store' }], '/ngwa/store');
		const sel = `.srow[data-id="${id}"]`;
		await waitFor(() => expect(document.querySelector(sel)).not.toBeNull());
		const row = document.querySelector(sel) as HTMLElement;
		fireEvent.click(row);
		await waitFor(() => expect(document.querySelector('[data-storesheet] .dhead')).not.toBeNull());
		return row;
	};
	const rowInstall = (row: HTMLElement) =>
		row.querySelector('.rt button.primary') as HTMLButtonElement;

	it('installs a skill pkg with an npx twin through the Ọba handler, at the catalog pin', async () => {
		remote.on = true;
		const row = await selectRegistryRow('@ikenga/skill-groundwork');
		// "also: npx" is the twin; the row's Install is live and says where from.
		expect(row.querySelector('[data-also]')?.textContent).toContain('npx');
		expect(rowInstall(row).disabled).toBe(false);
		expect(row.querySelector('[data-oba-route]')?.textContent).toMatch(/npx · pinned/);

		const install = await waitFor(() => {
			const b = document.querySelector('[data-storesheet] [data-install]') as HTMLButtonElement;
			expect(b).not.toBeNull();
			expect(b.disabled).toBe(false);
			return b;
		});
		expect(document.querySelector('[data-oba-note]')?.textContent).toContain(
			'Installs the skill into your account (npx source)'
		);
		expect(document.querySelector('[data-oba-foot]')?.textContent).toBe(
			'Installs the skill into your account (npx source)'
		);
		// The package's own signature row is not what is installed.
		expect(document.querySelector('[data-oba-trust]')).not.toBeNull();
		expect(document.querySelector('[data-pinned-version]')?.textContent).toContain(SHA.slice(0, 7));

		fireEvent.click(install);
		await waitFor(() => expect(m.obaInstallWithDeps).toHaveBeenCalledTimes(1));
		const args = m.obaInstallWithDeps.mock.calls[0];
		expect(args.slice(0, 4)).toEqual(['skill', 'groundwork', 'npx', 'royalti-io/groundwork']);
		// gitRef (a SHA is the pin, not a ref), fromCatalog, and the pin passed through.
		expect(args[5]).toBeNull();
		expect(args[6]).toBe(true);
		expect(args[7]).toEqual({ sha: SHA, hash: HASH });
		await waitFor(() =>
			expect(m.claudePrimitiveEnable).toHaveBeenCalledWith('skill', 'groundwork', 'project:p1')
		);
	});

	it('disables an APP in the row (with the reason) and in the sheet', async () => {
		remote.on = true;
		const row = await selectRegistryRow('@ikenga/pkg-tasks');
		const rowBtn = rowInstall(row);
		expect(rowBtn.disabled).toBe(true);
		expect(rowBtn.title).toBe('Packages are installed by the server operator');
		await waitFor(() => {
			const b = document.querySelector('[data-storesheet] [data-install]') as HTMLButtonElement;
			expect(b.disabled).toBe(true);
			expect(b.title).toBe('Packages are installed by the server operator');
		});
		expect(document.querySelector('[data-storesheet] [data-install-blocked]')?.textContent).toBe(
			'Packages are installed by the server operator'
		);
		expect(m.obaInstallWithDeps).not.toHaveBeenCalled();
	});

	it('keeps a skill pkg with no Ọba source disabled', async () => {
		remote.on = true;
		const row = await selectRegistryRow('@ikenga/skill-thunderbird');
		expect(row.querySelector('[data-also]')).toBeNull();
		expect(rowInstall(row).disabled).toBe(true);
		expect(rowInstall(row).title).toBe('Packages are installed by the server operator');
		await waitFor(() => {
			const b = document.querySelector('[data-storesheet] [data-install]') as HTMLButtonElement;
			expect(b.disabled).toBe(true);
		});
	});

	it('blocks a "skill" pkg that ships an MCP server once its manifest is read', async () => {
		remote.on = true;
		const row = await selectRegistryRow('@ikenga/skill-mcpish');
		await waitFor(() => {
			const b = document.querySelector('[data-storesheet] [data-install]') as HTMLButtonElement;
			expect(b.disabled).toBe(true);
			expect(b.title).toBe('Packages are installed by the server operator');
		});
		expect(document.querySelector('[data-oba-note]')).toBeNull();
		expect(m.obaInstallWithDeps).not.toHaveBeenCalled();
		// The cached manifest now also tells the list row.
		await waitFor(() => expect(rowInstall(row).disabled).toBe(true));
	});

	it('leaves registry pkgs as pkgs on the desktop', async () => {
		remote.on = false;
		const row = await selectRegistryRow('@ikenga/skill-groundwork');
		expect(rowInstall(row).disabled).toBe(false);
		expect(row.querySelector('[data-oba-route]')).toBeNull();
		expect(document.querySelector('[data-oba-note]')).toBeNull();
		const app = document.querySelector('.srow[data-id="@ikenga/pkg-tasks"] .rt button.primary');
		expect((app as HTMLButtonElement).disabled).toBe(false);
		await waitFor(() => {
			const b = document.querySelector('[data-storesheet] [data-install]') as HTMLButtonElement;
			expect(b.disabled).toBe(false);
		});
		expect(m.obaInstallWithDeps).not.toHaveBeenCalled();
	});
});
