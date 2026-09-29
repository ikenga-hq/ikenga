// /ngwa/installed — R57 flow 3 wired end to end: a vault-managed git item
// shows `<url> @ <sha>`, checks its remote only when its detail opens (Q4),
// updates to exactly the SHA shown through the confirm, and its Remove… is the
// dependents-aware safe delete — after Forget it shows as `local` (Q6).
// Every other item keeps the locked D-02 Update / Remove (covered by
// -installed-route.test.tsx).

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, screen, waitFor, within } from '@testing-library/react';
import type { NgwaItem } from '@ikenga/contract';
import * as cmd from '@/lib/tauri-cmd';
import * as home from '@/lib/home';
import { useShellStore } from '@/lib/shell/shell-store';
import { Route as InstalledRoute } from './installed';
import {
	HOME,
	PROJECTS,
	mkItem,
	mkPlacement,
	mkSnapshot,
	mountRoutes,
} from './-ngwa-test-fixtures';

vi.mock('@/lib/registry/use-registry', async (orig) => ({
	...(await orig<typeof import('@/lib/registry/use-registry')>()),
	useRegistryIndex: () => ({
		data: { index: { pkgs: [] }, indexUrl: 'https://registry.test/index.json' },
		isLoading: false,
		error: null,
	}),
}));

vi.mock('@/lib/ngwa/use-store-install', () => ({
	useStoreInstall: () => ({ install: vi.fn(), update: vi.fn(), updateAll: vi.fn() }),
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
		claudeStoreList: vi.fn(),
		obaCheckUpdate: vi.fn(),
		obaUpdate: vi.fn(),
		obaSetAutoUpdate: vi.fn(),
		obaDependents: vi.fn(),
		obaUnlinkOne: vi.fn(),
		obaSafeDelete: vi.fn(),
		obaForget: vi.fn(),
		obaRelinkDependents: vi.fn(),
		pkgKernelStatus: vi.fn(),
		pkgSettingsGet: vi.fn(),
		pkgPreviewManifest: vi.fn(),
	};
});

const m = vi.mocked(cmd);
vi.setConfig({ testTimeout: 20_000 });

const A = '3e1a9c0aaaa1111222233334444555566667777';
const B = '8b77f2d00112233445566778899aabbccddeeff0';
const MASTER = '/home/x/.local/share/ikenga/store/commands/release-status.md';
const LINK = '/home/x/.claude/commands/release-status.md';
const URL = 'https://github.com/ikenga-hq/claude-commands';

function releaseStatus(origin: Partial<NgwaItem['origin']> = {}): NgwaItem {
	return mkItem({
		id: 'command:personal:release-status',
		kind: 'command',
		name: 'release-status',
		display_name: '/release-status',
		version: A,
		origin: {
			source: 'git',
			url: URL,
			ref: 'main',
			resolved_version: A,
			publisher: null,
			managed: true,
			auto_update: false,
			installed_at_ms: 1,
			updated_at_ms: null,
			...origin,
		},
		placements: [
			mkPlacement({
				path: LINK,
				mechanism: 'file',
				in_store: true,
				link_target: MASTER,
				managed_by: 'oba',
			}),
		],
		install_path: MASTER,
	});
}
const lint = mkItem({ id: 'skill:personal:lint', kind: 'skill', name: 'lint' });

beforeEach(() => {
	vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')));
	m.ngwaSnapshot.mockResolvedValue(mkSnapshot([lint, releaseStatus()]));
	m.claudeStoreList.mockImplementation(async (kind) =>
		kind
			? []
			: [
					{
						kind: 'command',
						name: 'release-status',
						storePath: MASTER,
						canonicalPath: MASTER,
						description: null,
						modifiedMs: 0,
						enabledIn: ['workspace'],
						source: 'git',
						url: URL,
						ref: 'main',
						version: A,
						managed: true,
						fromCatalog: false,
						autoUpdate: false,
						pinned: true,
					},
				]
	);
	m.obaCheckUpdate.mockResolvedValue({ current: A, latest: B, behind: true });
	m.obaUpdate.mockResolvedValue({} as never);
	m.obaDependents.mockResolvedValue([LINK]);
	m.obaForget.mockImplementation(async () => {
		// The backend no longer records it: the next scan reports a local item.
		m.ngwaSnapshot.mockResolvedValue(
			mkSnapshot([
				lint,
				releaseStatus({
					source: 'local',
					url: null,
					ref: null,
					resolved_version: null,
					managed: false,
				}),
			])
		);
		return true;
	});
	m.pkgKernelStatus.mockResolvedValue({ registries: {} } as never);
	vi.mocked(home.loadHome).mockResolvedValue(HOME);
	useShellStore.setState({ projects: PROJECTS, activeProjectId: 'p1' } as never);
});

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	vi.unstubAllGlobals();
});

async function mountAndSelect(id: string) {
	mountRoutes([{ route: InstalledRoute, path: '/ngwa/installed' }], '/ngwa/installed');
	await screen.findByRole('listbox', { name: 'Installed equipment' });
	const row = await waitFor(() => {
		const el = document.querySelector<HTMLElement>(`.irow[data-id="${id}"]`);
		if (!el) throw new Error(`no row ${id}`);
		return el;
	});
	fireEvent.click(row);
	return waitFor(() => {
		const acts = document.querySelector<HTMLElement>('[data-dacts]');
		if (!acts) throw new Error('no action row');
		return acts;
	});
}
const updateBtn = (acts: HTMLElement) =>
	within(acts).getByRole('button', { name: /Update/ }) as HTMLButtonElement;

describe('/ngwa/installed — R57 git / npx items', () => {
	it('the detail line shows <url> @ <sha>, and the remote is checked only when the detail opens', async () => {
		mountRoutes([{ route: InstalledRoute, path: '/ngwa/installed' }], '/ngwa/installed');
		await screen.findByRole('listbox', { name: 'Installed equipment' });
		// The first row (lint) opens by default: no network for a git item
		// whose detail is not open.
		await waitFor(() => expect(document.querySelector('[data-dacts]')).not.toBeNull());
		expect(m.obaCheckUpdate).not.toHaveBeenCalled();
		cleanup();

		const acts = await mountAndSelect('command:personal:release-status');
		await waitFor(() =>
			expect(document.querySelector('[data-remote-line]')?.textContent).toBe(`${URL} @ 3e1a9c0`)
		);
		await waitFor(() => expect(m.obaCheckUpdate).toHaveBeenCalledWith('command', 'release-status'));
		await waitFor(() => expect(updateBtn(acts).disabled).toBe(false));
		expect(updateBtn(acts).title).toBe('3e1a9c0 → 8b77f2d at the remote');
	});

	it('up to date: Update is disabled with the newest-at-the-remote reason', async () => {
		m.obaCheckUpdate.mockResolvedValue({ current: A, latest: A, behind: false });
		const acts = await mountAndSelect('command:personal:release-status');
		await waitFor(() =>
			expect(updateBtn(acts).title).toBe('3e1a9c0 is the newest at the remote · checked just now')
		);
		expect(updateBtn(acts).disabled).toBe(true);
	});

	it('Update confirm fetches exactly the SHA shown', async () => {
		const acts = await mountAndSelect('command:personal:release-status');
		await waitFor(() => expect(updateBtn(acts).disabled).toBe(false));
		fireEvent.click(updateBtn(acts));
		const dialog = await screen.findByRole('dialog');
		expect(within(dialog).getByText('Update /release-status')).toBeDefined();
		await act(async () => {
			fireEvent.click(within(dialog).getByRole('button', { name: 'Update to 8b77f2d' }));
		});
		expect(m.obaUpdate).toHaveBeenCalledWith('command', 'release-status', { sha: B });
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(document.body.textContent).toContain('/release-status updated 3e1a9c0 → 8b77f2d');
	});

	it('Remove… opens the dependents-aware delete; Forget leaves the files and the item shows as local', async () => {
		const acts = await mountAndSelect('command:personal:release-status');
		fireEvent.click(within(acts).getByRole('button', { name: /Remove…/ }));
		const dialog = await screen.findByRole('dialog');
		await waitFor(() => expect(within(dialog).getByText('Linked into · 1')).toBeDefined());
		expect(m.obaDependents).toHaveBeenCalledWith('command', 'release-status');
		fireEvent.click(dialog.querySelector('[data-choice="forget"] input') as HTMLInputElement);
		await act(async () => {
			fireEvent.click(dialog.querySelector('[data-remove-confirm]') as HTMLElement);
		});
		expect(m.obaForget).toHaveBeenCalledWith('command', 'release-status');
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(document.body.textContent).toContain(
			'Forgot /release-status — its files and 1 link are untouched'
		);
		// Q6: it now reads as a local item — no remote line, the D-02 actions.
		await waitFor(() => expect(document.querySelector('[data-remote-line]')).toBeNull());
		const pane = document.querySelector('.detailcol') as HTMLElement;
		expect(pane.textContent).toContain('source: local');
		expect(m.obaSafeDelete).not.toHaveBeenCalled();
	});

	it('a non-remote item keeps the locked D-02 Update / Remove', async () => {
		const acts = await mountAndSelect('skill:personal:lint');
		expect(document.querySelector('[data-remote-line]')).toBeNull();
		expect(updateBtn(acts).title).toBe(
			'Not published in the registry, so there is nothing to update to'
		);
		expect(m.obaCheckUpdate).not.toHaveBeenCalled();
	});
});
