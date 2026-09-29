// R57 · Installed Update / Remove for vault-managed git / npx items. The
// confirm dialogs drive the Ọba commands directly (mocked here): Update fetches
// exactly the SHA shown and handles a pin mismatch distinctly; Remove reads the
// links from disk, then unlinks one / unlinks all and deletes / relinks and
// deletes / forgets — never undoable, always saying what happened.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import type { NgwaItem } from '@ikenga/contract';
import * as cmd from '@/lib/tauri-cmd';

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	obaUpdate: vi.fn(),
	obaSetAutoUpdate: vi.fn(),
	obaDependents: vi.fn(),
	obaUnlinkOne: vi.fn(),
	obaSafeDelete: vi.fn(),
	obaRelinkDependents: vi.fn(),
	obaForget: vi.fn(),
}));
const pick = vi.hoisted(() => vi.fn());
vi.mock('@/lib/transport/dialog-shim', () => ({ open: (...a: unknown[]) => pick(...a) }));

import { mkItem, mkPlacement } from '@/routes/ngwa/-ngwa-test-fixtures';
import {
	RemoteRemoveDialog,
	RemoteUpdateDialog,
	relinkCandidates,
	type NgwaRemoteRecord,
} from './ngwa-remote-dialogs';
import { placementTarget } from './ngwa-scope-model';

const m = vi.mocked(cmd);

const A = '3e1a9c0aaaa1111222233334444555566667777';
const B = '8b77f2d00112233445566778899aabbccddeeff0';
const MASTER = '/home/x/.local/share/ikenga/store/commands/release-status.md';
const L1 = '/home/x/.claude/commands/release-status.md';
const L2 = 'C:/Users/x/ikenga/.claude/commands/release-status.md';

const record = (over: Partial<NgwaRemoteRecord> = {}): NgwaRemoteRecord => ({
	kind: 'command',
	name: 'release-status',
	source: 'git',
	url: 'github.com/ikenga-hq/claude-commands',
	sha: A,
	ref: 'main',
	fromCatalog: false,
	pinned: true,
	hash: null,
	autoUpdate: false,
	master: MASTER,
	resolvedAtMs: Date.UTC(2026, 8, 20),
	catalogPin: null,
	catalogBehind: false,
	links: 2,
	...over,
});

const item: NgwaItem = mkItem({
	id: 'command:personal:release-status',
	kind: 'command',
	name: 'release-status',
	display_name: '/release-status',
	placements: [
		mkPlacement({
			path: L1,
			mechanism: 'file',
			in_store: true,
			link_target: MASTER,
			managed_by: 'oba',
		}),
	],
	required_by: [
		{ kind: 'workflow', name: 'release-status', item_id: null, source: null, ref: null },
	],
});
const projectCopy: NgwaItem = mkItem({
	id: 'command:project:p2:release-status',
	kind: 'command',
	name: 'release-status',
	scope: { kind: 'project', project_id: 'p2' },
	placements: [
		mkPlacement({
			path: 'C:/Users/x/other/.claude/commands/release-status.md',
			scope: { kind: 'project', project_id: 'p2' },
			mechanism: 'file',
		}),
	],
});
const scopeLabel = (k: string) => (k === 'personal' ? 'personal' : k.replace('project:', ''));
const placementPath = (p: NgwaItem['placements'][number]) => placementTarget(p, 'command');

beforeEach(() => {
	m.obaUpdate.mockResolvedValue({} as never);
	m.obaSetAutoUpdate.mockResolvedValue(true);
	m.obaDependents.mockResolvedValue([L1, L2]);
	m.obaUnlinkOne.mockResolvedValue(true);
	m.obaSafeDelete.mockResolvedValue({
		verdict: 'deleted',
		removed: true,
		dependents: [],
		message: 'ok',
	});
	m.obaRelinkDependents.mockResolvedValue([
		{ link: L1, ok: true, error: null },
		{ link: L2, ok: true, error: null },
	]);
	m.obaForget.mockResolvedValue(true);
});
afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('Update confirm', () => {
	function renderUpdate(rec = record(), target = { sha: B }) {
		const onClose = vi.fn();
		const onChanged = vi.fn();
		const onRecheck = vi.fn();
		render(
			<RemoteUpdateDialog
				request={{ item, record: rec, target, links: rec.links }}
				onClose={onClose}
				onChanged={onChanged}
				onRecheck={onRecheck}
			/>
		);
		return { onClose, onChanged, onRecheck, dialog: screen.getByRole('dialog') };
	}

	it('shows both SHAs, the swap, the trust line, and updates to exactly the SHA shown', async () => {
		const { dialog, onClose, onChanged } = renderUpdate();
		expect(within(dialog).getByText('added from a URL')).toBeDefined();
		expect(within(dialog).getAllByText('3e1a9c0').length).toBeGreaterThan(0);
		expect(within(dialog).getAllByText('8b77f2d').length).toBeGreaterThan(0);
		expect(within(dialog).getByText('HEAD of main')).toBeDefined();
		expect(within(dialog).getByText('2 — they keep pointing at the vault copy')).toBeDefined();
		expect(dialog.querySelector('[data-swap]')?.textContent).toContain(
			'Update fetches 8b77f2d into a staging folder and swaps it in one step. If the fetch fails, 3e1a9c0 stays exactly as it is.'
		);
		expect(dialog.querySelector('[data-trust-line]')?.textContent).toBe(
			'This source is not in the signed catalog. Nobody but you reviews what changed between 3e1a9c0 and 8b77f2d.'
		);
		expect(within(dialog).getByRole('button', { name: 'Keep 3e1a9c0' })).toBeDefined();

		await act(async () => {
			fireEvent.click(within(dialog).getByRole('button', { name: 'Update to 8b77f2d' }));
		});
		expect(m.obaUpdate).toHaveBeenCalledWith('command', 'release-status', { sha: B });
		expect(onChanged).toHaveBeenCalled();
		expect(onClose).toHaveBeenCalledWith({
			ok: true,
			text: '/release-status updated 3e1a9c0 → 8b77f2d — 2 links follow it',
		});
	});

	it('the auto-update box writes through obaSetAutoUpdate', async () => {
		const { dialog } = renderUpdate();
		const box = dialog.querySelector('[data-auto]') as HTMLInputElement;
		expect(box.checked).toBe(false);
		expect(dialog.textContent).toContain('Direct installs start with this off.');
		await act(async () => {
			fireEvent.click(box);
		});
		expect(m.obaSetAutoUpdate).toHaveBeenCalledWith('command', 'release-status', true);
		expect(box.checked).toBe(true);
	});

	it('a catalog install shows no trust line and its catalog pin', () => {
		const { dialog } = renderUpdate(
			record({ fromCatalog: true, catalogPin: { sha: B, hash: null }, catalogBehind: true })
		);
		expect(dialog.querySelector('[data-trust-line]')).toBeNull();
		expect(within(dialog).getByText('curated catalog')).toBeDefined();
		expect(within(dialog).getByText('moved by the signed catalog')).toBeDefined();
	});

	it('a pin mismatch is refused with nothing written, and offers a re-check', async () => {
		m.obaUpdate.mockRejectedValueOnce(
			new Error(`pin mismatch: the remote serves ${A.slice(0, 7)}ffff`)
		);
		const { dialog, onClose, onRecheck } = renderUpdate();
		await act(async () => {
			fireEvent.click(within(dialog).getByRole('button', { name: 'Update to 8b77f2d' }));
		});
		const err = dialog.querySelector('[data-update-error]') as HTMLElement;
		expect(err.textContent).toContain('The remote no longer serves 8b77f2d — nothing was written');
		expect(err.textContent).toContain('pin mismatch');
		expect(onClose).not.toHaveBeenCalled();
		fireEvent.click(within(dialog).getByRole('button', { name: 'Re-check the remote' }));
		expect(onRecheck).toHaveBeenCalledWith(expect.objectContaining({ name: 'release-status' }));
		expect(onClose).toHaveBeenCalledWith(null);
	});
});

describe('Remove… (dependents-aware safe delete)', () => {
	function renderRemove(items: NgwaItem[] = [item, projectCopy]) {
		const onClose = vi.fn();
		const onChanged = vi.fn();
		const onForgotten = vi.fn();
		render(
			<RemoteRemoveDialog
				request={{ item, record: record() }}
				items={items}
				scopeLabel={scopeLabel}
				placementPath={placementPath}
				onClose={onClose}
				onChanged={onChanged}
				onForgotten={onForgotten}
			/>
		);
		return { onClose, onChanged, onForgotten, dialog: screen.getByRole('dialog') };
	}

	it('opens on checking, read from disk, then lists links, required-by and the vault copy', async () => {
		let release: (v: string[]) => void = () => {};
		m.obaDependents.mockReturnValueOnce(new Promise((r) => (release = r)));
		const { dialog } = renderRemove();
		expect(dialog.querySelector('[data-remove-checking]')?.textContent).toContain(
			'Every scope and every engine is read from disk, not from a stored list'
		);
		expect(
			(within(dialog).getByRole('button', { name: 'Remove' }) as HTMLButtonElement).disabled
		).toBe(true);
		await act(async () => release([L1, L2]));
		expect(within(dialog).getByText('Linked into · 2')).toBeDefined();
		expect(dialog.querySelector(`[data-link="${L1}"]`)?.textContent).toContain('personal');
		expect(within(dialog).getByText('Required by · 1')).toBeDefined();
		expect(dialog.querySelector('[data-required-by]')?.textContent).toContain(
			'workflow — lists it in requires[]'
		);
		expect(dialog.querySelector('[data-vault-copy]')?.textContent).toContain(MASTER);
		expect(dialog.querySelector('[data-vault-copy]')?.textContent).toContain('managed · git');
	});

	it('Unlink one link from the list', async () => {
		const { dialog, onChanged } = renderRemove();
		await waitFor(() => expect(within(dialog).getByText('Linked into · 2')).toBeDefined());
		await act(async () => {
			fireEvent.click(dialog.querySelector(`[data-unlink="${L2}"]`) as HTMLElement);
		});
		expect(m.obaUnlinkOne).toHaveBeenCalledWith(L2);
		expect(within(dialog).getByText('Linked into · 1')).toBeDefined();
		expect(onChanged).toHaveBeenCalled();
	});

	it('Unlink N and delete needs the required-by acknowledgement, then unlinks each and deletes', async () => {
		const { dialog, onClose } = renderRemove();
		await waitFor(() => expect(within(dialog).getByText('Linked into · 2')).toBeDefined());
		expect(dialog.textContent).toContain(
			'This cannot be undone — reinstall from git · github.com/ikenga-hq/claude-commands to get it back.'
		);
		const confirm = dialog.querySelector('[data-remove-confirm]') as HTMLButtonElement;
		expect(confirm.textContent).toBe('Unlink 2 and delete');
		expect(confirm.disabled).toBe(true);
		expect(dialog.textContent).toContain(
			'release-status will lose something it requires. Remove anyway.'
		);
		fireEvent.click(dialog.querySelector('[data-ack]') as HTMLInputElement);
		expect(confirm.disabled).toBe(false);
		await act(async () => {
			fireEvent.click(confirm);
		});
		expect(m.obaUnlinkOne.mock.calls).toEqual([[L1], [L2]]);
		expect(m.obaSafeDelete).toHaveBeenCalledWith('command', 'release-status');
		expect(onClose).toHaveBeenCalledWith({
			ok: true,
			text: 'Removed /release-status — 2 links unlinked, vault copy deleted',
		});
	});

	it('refused_dependents re-lists the links and keeps the choice', async () => {
		m.obaSafeDelete.mockResolvedValueOnce({
			verdict: 'refused_dependents',
			removed: false,
			dependents: ['/new/link.md'],
			message: 'has dependents',
		});
		const { dialog, onClose } = renderRemove();
		await waitFor(() => expect(within(dialog).getByText('Linked into · 2')).toBeDefined());
		fireEvent.click(dialog.querySelector('[data-ack]') as HTMLInputElement);
		await act(async () => {
			fireEvent.click(dialog.querySelector('[data-remove-confirm]') as HTMLElement);
		});
		expect(onClose).not.toHaveBeenCalled();
		expect(within(dialog).getByText('Linked into · 1')).toBeDefined();
		expect(dialog.querySelector('[data-remove-note]')?.textContent).toContain(
			'your choice is kept'
		);
		expect((dialog.querySelector('[data-choice="unlink"] input') as HTMLInputElement).checked).toBe(
			true
		);
	});

	it('refused_external keeps the copy: external masters are never deleted', async () => {
		m.obaDependents.mockResolvedValueOnce([]);
		m.obaSafeDelete.mockResolvedValueOnce({
			verdict: 'refused_external',
			removed: false,
			dependents: [],
			message: 'external',
		});
		const { dialog, onClose } = renderRemove([item]);
		await waitFor(() => expect(within(dialog).getByText('Linked into · 0')).toBeDefined());
		fireEvent.click(dialog.querySelector('[data-ack]') as HTMLInputElement);
		await act(async () => {
			fireEvent.click(dialog.querySelector('[data-remove-confirm]') as HTMLElement);
		});
		expect(onClose).not.toHaveBeenCalled();
		expect(dialog.querySelector('[data-remove-note]')?.textContent).toContain(
			'External masters are never deleted by Ngwa'
		);
	});

	it('Relink points every link at the found copy, then deletes the vault copy', async () => {
		const { dialog, onClose } = renderRemove();
		await waitFor(() => expect(within(dialog).getByText('Linked into · 2')).toBeDefined());
		fireEvent.click(dialog.querySelector('[data-choice="relink"] input') as HTMLInputElement);
		expect(dialog.querySelector('[data-relink-target]')?.textContent).toContain(
			'C:/Users/x/other/.claude/commands/release-status.md'
		);
		// Relink keeps the requirement met, so it needs no acknowledgement.
		expect(dialog.querySelector('[data-ack]')).toBeNull();
		const confirm = dialog.querySelector('[data-remove-confirm]') as HTMLButtonElement;
		expect(confirm.textContent).toBe('Relink 2 and delete');
		await act(async () => {
			fireEvent.click(confirm);
		});
		expect(m.obaRelinkDependents).toHaveBeenCalledWith(
			[L1, L2],
			'C:/Users/x/other/.claude/commands/release-status.md'
		);
		expect(m.obaSafeDelete).toHaveBeenCalled();
		expect(onClose).toHaveBeenCalledWith(expect.objectContaining({ ok: true }));
	});

	it('Choose another folder… uses the folder picker; a failed relink keeps the vault copy', async () => {
		pick.mockResolvedValueOnce('/picked/release-status.md');
		m.obaRelinkDependents.mockResolvedValueOnce([
			{ link: L1, ok: true, error: null },
			{ link: L2, ok: false, error: 'permission denied' },
		]);
		const { dialog, onClose } = renderRemove([item]);
		await waitFor(() => expect(within(dialog).getByText('Linked into · 2')).toBeDefined());
		fireEvent.click(dialog.querySelector('[data-choice="relink"] input') as HTMLInputElement);
		expect((dialog.querySelector('[data-remove-confirm]') as HTMLButtonElement).disabled).toBe(
			true
		);
		await act(async () => {
			fireEvent.click(dialog.querySelector('[data-pickcopy]') as HTMLElement);
		});
		expect(pick).toHaveBeenCalledWith(
			expect.objectContaining({ directory: false, multiple: false })
		);
		expect(dialog.querySelector('[data-relink-target]')?.textContent).toContain(
			'/picked/release-status.md'
		);
		await act(async () => {
			fireEvent.click(dialog.querySelector('[data-remove-confirm]') as HTMLElement);
		});
		expect(m.obaSafeDelete).not.toHaveBeenCalled();
		expect(onClose).not.toHaveBeenCalled();
		expect(dialog.querySelector('[data-relink-failures]')?.textContent).toContain(
			'permission denied'
		);
	});

	it('Forget drops the record only, touches no files, and marks the item local', async () => {
		const { dialog, onClose, onForgotten } = renderRemove();
		await waitFor(() => expect(within(dialog).getByText('Linked into · 2')).toBeDefined());
		fireEvent.click(dialog.querySelector('[data-choice="forget"] input') as HTMLInputElement);
		const confirm = dialog.querySelector('[data-remove-confirm]') as HTMLButtonElement;
		expect(confirm.textContent).toBe('Forget');
		expect(confirm.disabled).toBe(false);
		await act(async () => {
			fireEvent.click(confirm);
		});
		expect(m.obaForget).toHaveBeenCalledWith('command', 'release-status');
		expect(m.obaUnlinkOne).not.toHaveBeenCalled();
		expect(m.obaSafeDelete).not.toHaveBeenCalled();
		expect(onForgotten).toHaveBeenCalledWith(expect.objectContaining({ name: 'release-status' }));
		expect(onClose).toHaveBeenCalledWith({
			ok: true,
			text: 'Forgot /release-status — its files and 2 links are untouched',
		});
	});
});

describe('relinkCandidates', () => {
	it('lists same-named real copies in other scopes, never store links or the master', () => {
		const out = relinkCandidates([item, projectCopy], record(), scopeLabel, placementPath);
		expect(out).toEqual([
			{ path: 'C:/Users/x/other/.claude/commands/release-status.md', label: 'p2' },
		]);
	});
});
