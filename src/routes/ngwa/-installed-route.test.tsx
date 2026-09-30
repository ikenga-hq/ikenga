// /ngwa/installed + /ngwa/item/$itemId route tests: the D-02 detail action row
// and row context menu, and the D-08 item header + ⋯ menu, wired through the
// shared Ngwa actions. `tauri-cmd` is mocked, so no command touches disk; each
// test clicks a control and asserts the exact writer + arguments (pkg vs Ọba
// primitive, scope mapping personal → 'workspace', project → `project:<id>`).

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, screen, waitFor, within } from '@testing-library/react';
import * as cmd from '@/lib/tauri-cmd';
import * as home from '@/lib/home';
import { useShellStore } from '@/lib/shell/shell-store';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useCompanionStore } from '@/shell/companion/companion-store';
import { NeedsApprovalError } from '@/lib/ngwa/use-store-install';
import type { NgwaStoreEntry } from '@/lib/ngwa/enrichment';
import { Route as InstalledRoute } from './installed';
import { Route as ItemRoute } from './item.$itemId';
import { HOME, PROJECTS, mkSnapshot, mountRoutes, scopesItems } from './-ngwa-test-fixtures';

const registryPkgs = vi.hoisted(() => ({
	list: [] as Array<{ name: string; latest: string }>,
}));
const storeUpdate = vi.hoisted(() => vi.fn());

vi.mock('@/lib/registry/use-registry', async (orig) => ({
	...(await orig<typeof import('@/lib/registry/use-registry')>()),
	useRegistryIndex: () => ({
		data: { index: { pkgs: registryPkgs.list }, indexUrl: 'https://registry.test/index.json' },
		isLoading: false,
		error: null,
	}),
}));

vi.mock('@/lib/ngwa/use-store-install', async (orig) => ({
	...(await orig<typeof import('@/lib/ngwa/use-store-install')>()),
	useStoreInstall: () => ({ install: vi.fn(), update: storeUpdate, updateAll: vi.fn() }),
}));

vi.mock('@/lib/home', () => ({
	loadHome: vi.fn(),
	getHomeSync: () => '',
	shortPath: (p: string) => p,
}));

vi.mock('@/lib/tauri-cmd', async (orig) => {
	const actual = await orig<typeof import('@/lib/tauri-cmd')>();
	return {
		...actual,
		ngwaSnapshot: vi.fn(),
		claudePrimitiveEnable: vi.fn(),
		claudePrimitiveDisable: vi.fn(),
		claudePrimitiveCopy: vi.fn(),
		claudePrimitiveMove: vi.fn(),
		claudePrimitiveRemove: vi.fn(),
		claudePrimitiveEnableFor: vi.fn(),
		claudePrimitiveDisableFor: vi.fn(),
		pkgSetEnabled: vi.fn(),
		pkgUninstall: vi.fn(),
		pkgKernelStatus: vi.fn(),
		pkgSettingsGet: vi.fn(),
		pkgSettingsSet: vi.fn(),
		pkgTrustGrant: vi.fn(),
		pkgTrustRevoke: vi.fn(),
		pkgPreviewManifest: vi.fn(),
	};
});

const m = vi.mocked(cmd);

// Each test mounts the real routes in a real router; under a loaded full-suite
// run a mount can pass the 5 s default, which is not what these tests check.
vi.setConfig({ testTimeout: 20_000 });

const WRITES = [
	() => m.claudePrimitiveEnable,
	() => m.claudePrimitiveDisable,
	() => m.claudePrimitiveCopy,
	() => m.claudePrimitiveMove,
	() => m.claudePrimitiveRemove,
	() => m.pkgSetEnabled,
	() => m.pkgUninstall,
	() => m.pkgSettingsSet,
];
const noWrites = () => {
	for (const f of WRITES) expect(f()).not.toHaveBeenCalled();
};

beforeEach(() => {
	registryPkgs.list = [{ name: 'com.ikenga.tasks', latest: '0.8.2' }];
	m.ngwaSnapshot.mockResolvedValue(mkSnapshot(scopesItems()));
	vi.mocked(home.loadHome).mockResolvedValue(HOME);
	// biome-ignore lint/suspicious/noExplicitAny: heterogeneous mocks
	for (const f of WRITES) (f() as any).mockResolvedValue(undefined);
	m.pkgKernelStatus.mockResolvedValue({
		registries: {
			views: {
				entries: [
					{
						pkg_id: 'com.ikenga.tasks',
						pkg_name: 'Tasks',
						qualified_id: 'com.ikenga.tasks:board',
						id: 'board',
						title: 'Board',
						route: '/board',
						pane_route: '/pkg/com.ikenga.tasks/board',
						pin_on_install: false,
					},
				],
			},
		},
	} as never);
	m.pkgSettingsGet.mockResolvedValue({
		pkg_id: 'com.ikenga.tasks',
		schema: [
			{ key: 'columns', type: 'number', label: 'Columns', default: 4 },
			{ key: 'api_token', type: 'secret', label: 'API token', default: '' },
			{ key: 'board', type: 'string', label: 'Board' },
		],
		values: { columns: 6 },
	});
	m.pkgPreviewManifest.mockResolvedValue({ id: 'x', workflows: [] } as never);
	useShellStore.setState({ projects: PROJECTS, activeProjectId: 'p1' } as never);
	useCompanionStore.getState().setDraft('');
});

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

async function mountInstalled() {
	const r = mountRoutes(
		[
			{ route: InstalledRoute, path: '/ngwa/installed' },
			{ route: ItemRoute, path: '/ngwa/item/$itemId' },
		],
		'/ngwa/installed'
	);
	await screen.findByRole('listbox', { name: 'Installed equipment' });
	await waitFor(() => expect(home.loadHome).toHaveBeenCalled());
	await act(async () => {});
	return r;
}

function row(id: string): HTMLElement {
	const el = document.querySelector<HTMLElement>(`.irow[data-id="${id}"]`);
	if (!el) throw new Error(`no row ${id}`);
	return el;
}

async function select(id: string) {
	fireEvent.click(await waitFor(() => row(id)));
	return waitFor(() => {
		const acts = document.querySelector<HTMLElement>('[data-dacts]');
		if (!acts) throw new Error('no action row');
		return acts;
	});
}

const REVIEW = {
	pkg_id: 'com.ikenga.tasks',
	manifest_version: '0.9.0',
	old_capabilities: '{}',
	new_capabilities: '{"net":["https://api.example.com"]}',
	prior_approved_at_ms: 0,
};

const btn = (scope: HTMLElement, name: RegExp) =>
	within(scope).getByRole('button', { name }) as HTMLButtonElement;
const dialog = () => screen.getByRole('dialog');

describe('/ngwa/installed — D-02 detail action row', () => {
	it('draws the designed row: Disable · Move… · Copy to… · Update · Open folder · Remove… · Hand to Chi', async () => {
		await mountInstalled();
		const acts = await select('com.ikenga.tasks');
		const labels = within(acts)
			.getAllByRole('button')
			.map((b) => b.textContent?.trim());
		expect(labels).toEqual([
			'Disable',
			'Move…',
			'Copy to…',
			'Update',
			'Open folder',
			'Remove…',
			'Hand to Chi',
		]);
		expect(btn(acts, /Remove…/).className).toContain('danger');
		expect(btn(acts, /Hand to Chi/).className).toContain('on');
	});

	it('toggle on a pkg calls pkgSetEnabled; on a skill calls the Ọba writer for its scope', async () => {
		await mountInstalled();
		let acts = await select('com.ikenga.tasks');
		fireEvent.click(btn(acts, /^Disable$/));
		await waitFor(() => expect(m.pkgSetEnabled).toHaveBeenCalledWith('com.ikenga.tasks', false));
		expect(m.claudePrimitiveDisable).not.toHaveBeenCalled();

		acts = await select('com.ikenga.studio');
		await waitFor(() => expect(btn(acts, /^Enable$/).disabled).toBe(false));
		fireEvent.click(btn(acts, /^Enable$/));
		await waitFor(() => expect(m.pkgSetEnabled).toHaveBeenCalledWith('com.ikenga.studio', true));

		acts = await select('skill:personal:lint');
		await waitFor(() => expect(btn(acts, /^Disable$/).disabled).toBe(false));
		fireEvent.click(btn(acts, /^Disable$/));
		await waitFor(() =>
			expect(m.claudePrimitiveDisable).toHaveBeenCalledWith('skill', 'lint', 'workspace')
		);
		expect(m.pkgSetEnabled).toHaveBeenCalledTimes(2);
	});

	it('Disable on a real (non-link) skill folder is blocked with the Scopes reason', async () => {
		await mountInstalled();
		const acts = await select('skill:personal:notes');
		const b = btn(acts, /^Disable$/);
		expect(b.disabled).toBe(true);
		expect(b.title).toMatch(/real folder, not a store link/);
	});

	it('Remove… on a pkg confirms (D-02 copy), Keep it calls nothing, Remove anyway calls pkgUninstall', async () => {
		await mountInstalled();
		const acts = await select('com.ikenga.tasks');
		fireEvent.click(btn(acts, /Remove…/));
		expect(within(dialog()).getByText('Remove Tasks')).toBeTruthy();
		expect(dialog().textContent).toMatch(/so no closure breaks/);
		expect(dialog().textContent).toMatch(/left on disk/);
		fireEvent.click(within(dialog()).getByRole('button', { name: 'Keep it' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		noWrites();

		fireEvent.click(btn(acts, /Remove…/));
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Remove anyway' }));
		});
		await waitFor(() => expect(m.pkgUninstall).toHaveBeenCalledWith('com.ikenga.tasks'));
		expect(m.claudePrimitiveRemove).not.toHaveBeenCalled();
	});

	it('Remove… on a skill names the exact path, then calls the Ọba remove for its scope', async () => {
		await mountInstalled();
		const acts = await select('skill:personal:lint');
		await waitFor(() => expect(btn(acts, /Remove…/).disabled).toBe(false));
		fireEvent.click(btn(acts, /Remove…/));
		expect(dialog().querySelector('[data-remove-path]')?.textContent).toBe(
			`${HOME}/.claude/skills/lint`
		);
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Remove anyway' }));
		});
		await waitFor(() =>
			expect(m.claudePrimitiveRemove).toHaveBeenCalledWith('skill', 'lint', 'workspace')
		);
		expect(m.pkgUninstall).not.toHaveBeenCalled();
	});

	it('Remove… on a builtin pkg is disabled with the Scopes wording', async () => {
		await mountInstalled();
		const acts = await select('com.ikenga.engine-claude-code');
		const b = btn(acts, /Remove…/);
		expect(b.disabled).toBe(true);
		expect(b.title).toBe('Shipped with the shell and cannot be uninstalled; disable it instead');
	});

	it('Update is disabled with "<ver> is the newest published version" when current', async () => {
		await mountInstalled();
		const acts = await select('com.ikenga.tasks');
		const b = btn(acts, /^Update$/);
		expect(b.disabled).toBe(true);
		expect(b.title).toBe('0.8.2 is the newest published version');
	});

	it('Update runs the Store update path when a newer version is published', async () => {
		registryPkgs.list = [{ name: 'com.ikenga.tasks', latest: '0.9.0' }];
		await mountInstalled();
		const acts = await select('com.ikenga.tasks');
		const b = btn(acts, /^Update$/);
		expect(b.disabled).toBe(false);
		expect(b.title).toBe('Update to 0.9.0');
		fireEvent.click(b);
		await waitFor(() => expect(storeUpdate).toHaveBeenCalledTimes(1));
		expect(storeUpdate.mock.calls[0][0]).toMatchObject({
			latestVersion: '0.9.0',
			installedItem: { id: 'com.ikenga.tasks' },
		});
	});

	it('Update that asks for new permissions opens the trust review; Approve re-runs it approved', async () => {
		registryPkgs.list = [{ name: 'com.ikenga.tasks', latest: '0.9.0' }];
		storeUpdate.mockImplementation((entry: NgwaStoreEntry, opts?: { approved?: boolean }) =>
			opts?.approved
				? Promise.resolve()
				: Promise.reject(new NeedsApprovalError([{ entry, review: REVIEW }]))
		);
		await mountInstalled();
		const acts = await select('com.ikenga.tasks');
		fireEvent.click(btn(acts, /^Update$/));

		// The updater's own modal, not a failure status.
		const rowEl = await screen.findByTestId('trust-review-row-com.ikenga.tasks');
		expect(within(dialog()).getByText('Capability review')).toBeTruthy();
		expect(screen.queryByText(/failed/)).toBeNull();
		expect(screen.queryByText(/from the Installed tab/)).toBeNull();

		await act(async () => {
			fireEvent.click(within(rowEl).getByTestId('trust-review-approve-com.ikenga.tasks'));
		});
		await waitFor(() => expect(storeUpdate).toHaveBeenCalledTimes(2));
		expect(storeUpdate.mock.calls[1][1]).toEqual({ approved: true });
		expect(storeUpdate.mock.calls[1][0]).toMatchObject({ latestVersion: '0.9.0' });
		await waitFor(() => expect(screen.queryByText('Capability review')).toBeNull());
		expect(await screen.findByText(/^Updated .+ to 0.9.0$/)).toBeTruthy();
	});

	it('Reject in the trust review leaves the pkg un-updated', async () => {
		registryPkgs.list = [{ name: 'com.ikenga.tasks', latest: '0.9.0' }];
		storeUpdate.mockImplementation((entry: NgwaStoreEntry) =>
			Promise.reject(new NeedsApprovalError([{ entry, review: REVIEW }]))
		);
		await mountInstalled();
		const acts = await select('com.ikenga.tasks');
		fireEvent.click(btn(acts, /^Update$/));
		await screen.findByTestId('trust-review-row-com.ikenga.tasks');
		await act(async () => {
			fireEvent.click(screen.getByTestId('trust-review-reject-com.ikenga.tasks'));
		});
		await waitFor(() => expect(screen.queryByText('Capability review')).toBeNull());
		// Only the held first attempt ran; nothing was installed on Reject.
		expect(storeUpdate).toHaveBeenCalledTimes(1);
		noWrites();
	});

	it('Move… offers each scope; a confirmed move calls the Ọba move from the item scope', async () => {
		await mountInstalled();
		const acts = await select('skill:personal:lint');
		await waitFor(() => expect(btn(acts, /Move…/).disabled).toBe(false));
		fireEvent.click(btn(acts, /Move…/));
		const menu = screen.getByRole('menu', { name: 'Move to' });
		const here = within(menu).getByRole('menuitem', { name: /Personal/ }) as HTMLButtonElement;
		expect(here.disabled).toBe(true);
		expect(here.title).toBe('Already here');
		fireEvent.click(within(menu).getByRole('menuitem', { name: /royalti-co/ }));
		expect(within(dialog()).getByText('Move lint to royalti-co')).toBeTruthy();
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Move' }));
		});
		await waitFor(() =>
			expect(m.claudePrimitiveMove).toHaveBeenCalledWith('skill', 'lint', 'workspace', 'project:p1')
		);
	});

	it('Move… and Copy to… are disabled for a pkg: it lives in exactly one scope', async () => {
		await mountInstalled();
		const acts = await select('com.ikenga.tasks');
		expect(btn(acts, /Move…/).disabled).toBe(true);
		expect(btn(acts, /Copy to…/).title).toBe('A pkg lives in exactly one scope');
	});

	it('Hand to Chi fills the Companion dispatch bar (never sends)', async () => {
		await mountInstalled();
		const acts = await select('com.ikenga.tasks');
		fireEvent.click(btn(acts, /Hand to Chi/));
		expect(useCompanionStore.getState().draft).toBe('Look at /pkgs/tasks');
		noWrites();
	});
});

describe('/ngwa/installed — D-02 row context menu', () => {
	it('opens on right-click with the designed items', async () => {
		await mountInstalled();
		fireEvent.contextMenu(await waitFor(() => row('com.ikenga.tasks')));
		const menu = screen.getByRole('menu', { name: 'Tasks' });
		const items = within(menu)
			.getAllByRole('menuitem')
			.map((b) => b.textContent);
		expect(items).toEqual([
			'DisableSpace',
			'Move to project',
			'Move to personal',
			'Copy to…',
			'Update',
			'Remove…',
			'Open folder↵',
			'Hand to Chi',
		]);
		// The name is the group header.
		expect(menu.querySelector('.mgroup')?.textContent).toBe('Tasks');
		// A project-scoped pkg: Move to project says why it is disabled.
		const mp = within(menu).getByRole('menuitem', { name: 'Move to project' }) as HTMLButtonElement;
		expect(mp.disabled).toBe(true);
		expect(mp.title).toBeTruthy();
	});

	it('Update from the context menu routes a permissions hold to the same trust review', async () => {
		registryPkgs.list = [{ name: 'com.ikenga.tasks', latest: '0.9.0' }];
		storeUpdate.mockImplementation((entry: NgwaStoreEntry, opts?: { approved?: boolean }) =>
			opts?.approved
				? Promise.resolve()
				: Promise.reject(new NeedsApprovalError([{ entry, review: REVIEW }]))
		);
		await mountInstalled();
		fireEvent.contextMenu(await waitFor(() => row('com.ikenga.tasks')));
		const menu = screen.getByRole('menu', { name: 'Tasks' });
		fireEvent.click(within(menu).getByRole('menuitem', { name: /^Update/ }));
		await screen.findByTestId('trust-review-row-com.ikenga.tasks');
		await act(async () => {
			fireEvent.click(screen.getByTestId('trust-review-approve-com.ikenga.tasks'));
		});
		await waitFor(() => expect(storeUpdate).toHaveBeenCalledTimes(2));
		expect(storeUpdate.mock.calls[1][1]).toEqual({ approved: true });
	});

	it('Move to personal is disabled "Already personal" for a personal skill; Move to project moves it', async () => {
		await mountInstalled();
		fireEvent.contextMenu(await waitFor(() => row('skill:personal:lint')));
		const menu = screen.getByRole('menu', { name: 'lint' });
		const mp = within(menu).getByRole('menuitem', {
			name: 'Move to personal',
		}) as HTMLButtonElement;
		expect(mp.disabled).toBe(true);
		expect(mp.title).toBe('Already personal');
		fireEvent.click(within(menu).getByRole('menuitem', { name: 'Move to project' }));
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Move' }));
		});
		await waitFor(() =>
			expect(m.claudePrimitiveMove).toHaveBeenCalledWith('skill', 'lint', 'workspace', 'project:p1')
		);
	});

	it('Copy to… swaps the menu for the scope picker and copies after the confirm', async () => {
		await mountInstalled();
		fireEvent.contextMenu(await waitFor(() => row('skill:personal:lint')));
		fireEvent.click(
			within(screen.getByRole('menu', { name: 'lint' })).getByRole('menuitem', { name: 'Copy to…' })
		);
		const picker = screen.getByRole('menu', { name: 'Copy to' });
		fireEvent.click(within(picker).getByRole('menuitem', { name: /royalti-co/ }));
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Copy' }));
		});
		await waitFor(() =>
			expect(m.claudePrimitiveCopy).toHaveBeenCalledWith('skill', 'lint', 'workspace', 'project:p1')
		);
	});
});

describe('/ngwa/item/$itemId — D-08 header', () => {
	async function mountItem(id: string) {
		const r = mountRoutes([{ route: ItemRoute, path: '/ngwa/item/$itemId' }], `/ngwa/item/${id}`);
		await screen.findByRole('tablist');
		await waitFor(() => expect(home.loadHome).toHaveBeenCalled());
		await act(async () => {});
		return r;
	}
	const header = () => document.querySelector<HTMLElement>('[data-idacts]') as HTMLElement;

	it('pkg header is Open view · Disable · ⋯ — no Uninstall, Update or Hand to Chi', async () => {
		await mountItem('com.ikenga.tasks');
		await waitFor(() =>
			expect(within(header()).getByRole('button', { name: /Open view/ })).toBeTruthy()
		);
		const labels = within(header())
			.getAllByRole('button')
			.map((b) => b.getAttribute('aria-label') ?? b.textContent?.trim());
		expect(labels).toEqual(['Open view', 'Disable', 'More']);
		expect(
			within(header()).queryByRole('button', { name: /Uninstall|Update|Hand to Chi/ })
		).toBeNull();
	});

	it('Open view navigates the focused pane to the first ui.views route; Disable calls pkgSetEnabled', async () => {
		const nav = vi.spyOn(usePaneStore.getState(), 'navigateFocused').mockImplementation(() => {});
		await mountItem('com.ikenga.tasks');
		fireEvent.click(
			await waitFor(() => within(header()).getByRole('button', { name: /Open view/ }))
		);
		expect(nav).toHaveBeenCalledWith('/pkg/com.ikenga.tasks/board');
		fireEvent.click(within(header()).getByRole('button', { name: /^Disable$/ }));
		await waitFor(() => expect(m.pkgSetEnabled).toHaveBeenCalledWith('com.ikenga.tasks', false));
		nav.mockRestore();
	});

	it('⋯ opens itemDotsMenu: Open manifest.json · Reveal install path · Reset settings (danger) · Copy as iyke', async () => {
		await mountItem('com.ikenga.tasks');
		fireEvent.click(within(header()).getByRole('button', { name: 'More' }));
		const menu = screen.getByRole('menu', { name: 'More' });
		expect(
			within(menu)
				.getAllByRole('menuitem')
				.map((b) => b.textContent)
		).toEqual([
			'Open manifest.json',
			'Reveal install path',
			'Reset settings to defaults',
			'Copy as iyke',
		]);
		expect(
			within(menu).getByRole('menuitem', { name: 'Reset settings to defaults' }).className
		).toContain('danger');
		expect(document.querySelector('[data-iykeline] .cmdtext')?.textContent).toBe(
			'ngwa item com.ikenga.tasks'
		);
	});

	it('Open manifest.json opens it as a pane tab; Reveal install path reveals it in Files', async () => {
		const addTab = vi.spyOn(usePaneStore.getState(), 'addTab').mockImplementation(() => {});
		const reveal = vi.spyOn(usePaneStore.getState(), 'revealPath').mockImplementation(() => {});
		await mountItem('com.ikenga.tasks');
		fireEvent.click(within(header()).getByRole('button', { name: 'More' }));
		fireEvent.click(screen.getByRole('menuitem', { name: 'Open manifest.json' }));
		expect(addTab).toHaveBeenCalledWith(expect.any(String), {
			kind: 'artifact',
			path: '/pkgs/tasks/manifest.json',
		});
		fireEvent.click(within(header()).getByRole('button', { name: 'More' }));
		fireEvent.click(screen.getByRole('menuitem', { name: 'Reveal install path' }));
		expect(reveal).toHaveBeenCalledWith('/pkgs/tasks');
		addTab.mockRestore();
		reveal.mockRestore();
	});

	it('Reset settings confirms, then writes each non-secret default (and only those)', async () => {
		await mountItem('com.ikenga.tasks');
		fireEvent.click(within(header()).getByRole('button', { name: 'More' }));
		const reset = await waitFor(() => {
			const el = screen.getByRole('menuitem', {
				name: 'Reset settings to defaults',
			}) as HTMLButtonElement;
			if (el.disabled) throw new Error('still loading');
			return el;
		});
		fireEvent.click(reset);
		expect(within(dialog()).getByText('Reset Tasks’s settings?')).toBeTruthy();
		expect(dialog().textContent).toMatch(/Vault keys are not touched/);
		expect(dialog().querySelector('[data-reset-kept]')?.textContent).toMatch(/Board/);
		await act(async () => {
			fireEvent.click(within(dialog()).getByRole('button', { name: 'Reset' }));
		});
		await waitFor(() => expect(m.pkgSettingsSet).toHaveBeenCalledTimes(1));
		expect(m.pkgSettingsSet).toHaveBeenCalledWith('com.ikenga.tasks', 'columns', 4);
	});

	it('skill variant: Brief a Chi · ⋯, no Disable; ⋯ explains what a skill lacks', async () => {
		await mountItem('lint');
		const labels = within(header())
			.getAllByRole('button')
			.map((b) => b.getAttribute('aria-label') ?? b.textContent?.trim());
		expect(labels).toEqual(['Brief a Chi', 'More']);
		fireEvent.click(within(header()).getByRole('button', { name: /Brief a Chi/ }));
		expect(useCompanionStore.getState().draft).toBe('/lint ');
		fireEvent.click(within(header()).getByRole('button', { name: 'More' }));
		const manifest = screen.getByRole('menuitem', {
			name: 'Open manifest.json',
		}) as HTMLButtonElement;
		expect(manifest.disabled).toBe(true);
		expect(manifest.title).toBe('A skill has no manifest.json');
		const reset = screen.getByRole('menuitem', {
			name: 'Reset settings to defaults',
		}) as HTMLButtonElement;
		expect(reset.disabled).toBe(true);
		noWrites();
	});
});
