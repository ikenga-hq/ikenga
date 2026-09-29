// R57 · the Installed Update for git / npx items: which record an item has,
// and which Update it gets for each check result (Q4 — a catalog install
// compares with its catalog pin, a direct one follows its on-open check; hook
// and MCP entries can't be updated in place).

import { describe, expect, it, vi } from 'vitest';
import type { NgwaItem } from '@ikenga/contract';
import type { PrimitiveCatalogEntry } from '@/lib/registry/primitives';
import type { ClaudeStoreEntry } from '@/lib/tauri-cmd';
import { mkItem, mkPlacement } from '@/routes/ngwa/-ngwa-test-fixtures';
import {
	isRemoteItem,
	remoteRecordOf,
	remoteUpdateAct,
	toRemoteCheck,
	type RemoteCheck,
} from './use-ngwa-actions';

const A = '3e1a9c0aaaa1111222233334444555566667777';
const B = '8b77f2d00112233445566778899aabbccddeeff0';
const gate = (why: string | undefined) => why;

const remote = (over: Partial<NgwaItem> = {}, origin: Partial<NgwaItem['origin']> = {}) =>
	mkItem({
		id: 'skill:personal:design-language',
		kind: 'skill',
		name: 'design-language',
		origin: {
			source: 'npx',
			url: 'royalti-io/design-language',
			ref: null,
			resolved_version: A,
			publisher: null,
			managed: true,
			auto_update: true,
			installed_at_ms: 1,
			updated_at_ms: 2,
			...origin,
		},
		placements: [
			mkPlacement({ path: '/h/.claude/skills/design-language/SKILL.md', in_store: true }),
		],
		...over,
	});
const vault = (over: Partial<ClaudeStoreEntry> = {}): ClaudeStoreEntry => ({
	kind: 'skill',
	name: 'design-language',
	storePath: '/vault/skills/design-language',
	description: null,
	modifiedMs: 0,
	enabledIn: ['workspace'],
	version: A,
	fromCatalog: true,
	...over,
});
const catalog = (ref?: string): PrimitiveCatalogEntry[] => [
	{
		kind: 'skill',
		name: 'design-language',
		version: '0.1.0',
		description: null,
		source: 'npx',
		url: 'royalti-io/design-language',
		...(ref ? { ref } : {}),
	},
];
const ok = (latest: string, behind: boolean): RemoteCheck => ({
	status: 'ok',
	current: A,
	latest,
	behind,
	error: null,
	checkedAtMs: Date.now(),
});

describe('isRemoteItem / remoteRecordOf', () => {
	it('only a vault-managed git / npx primitive is remote', () => {
		expect(isRemoteItem(remote())).toBe(true);
		expect(isRemoteItem(remote({}, { managed: false }))).toBe(false);
		expect(isRemoteItem(remote({}, { source: 'local' }))).toBe(false);
		expect(isRemoteItem(remote({ kind: 'app', id: 'com.x' }, { source: 'git' }))).toBe(false);
	});

	it('joins the vault record and the catalog pin', () => {
		const it = remote();
		const rec = remoteRecordOf(it, 'skill', [vault()], catalog(B), [it]);
		expect(rec).toMatchObject({
			kind: 'skill',
			source: 'npx',
			url: 'royalti-io/design-language',
			sha: A,
			fromCatalog: true,
			catalogPin: { sha: B, hash: null },
			catalogBehind: true,
			master: '/vault/skills/design-language',
			links: 1,
		});
		// A direct install of the same name does not take the catalog pin.
		const direct = remoteRecordOf(it, 'skill', [vault({ fromCatalog: false })], catalog(B), [it]);
		expect(direct?.catalogPin).toBeNull();
		expect(remoteRecordOf(remote({}, { managed: false }), 'skill', [], [], [])).toBeNull();
	});
});

describe('remoteUpdateAct', () => {
	const rec = (over = {}) => ({
		...(remoteRecordOf(remote(), 'skill', [vault({ fromCatalog: false })], [], []) as NonNullable<
			ReturnType<typeof remoteRecordOf>
		>),
		...over,
	});

	it('a catalog install moves to the catalog pin, without asking the remote', () => {
		const open = vi.fn();
		const r = remoteRecordOf(remote(), 'skill', [vault()], catalog(B), []);
		const act = remoteUpdateAct(r as NonNullable<typeof r>, null, open, gate);
		expect(act.label).toBe('Update to 8b77f2d');
		expect(act.title).toBe('3e1a9c0 → 8b77f2d in the signed catalog');
		expect(act.disabledReason).toBeUndefined();
		act.run();
		expect(open).toHaveBeenCalledWith({ sha: B, hash: null });

		const at = remoteRecordOf(remote(), 'skill', [vault()], catalog(A), []);
		expect(remoteUpdateAct(at as NonNullable<typeof at>, null, open, gate).disabledReason).toBe(
			"3e1a9c0 is the signed catalog's pinned version"
		);
	});

	it('a direct install follows the check: idle, loading, error, behind, current', () => {
		const open = vi.fn();
		const r = rec();
		expect(remoteUpdateAct(r, null, open, gate).disabledReason).toBe(
			'Open the item to check the remote'
		);
		expect(
			remoteUpdateAct(r, toRemoteCheck({ status: 'pending', fetchStatus: 'fetching' }), open, gate)
				.disabledReason
		).toBe('Checking the remote…');
		expect(
			remoteUpdateAct(
				r,
				toRemoteCheck({ status: 'error', error: new Error('ls-remote failed') }),
				open,
				gate
			).disabledReason
		).toBe("Couldn't check the remote: ls-remote failed");
		const behind = remoteUpdateAct(r, ok(B, true), open, gate);
		expect(behind.label).toBe('Update to 8b77f2d');
		expect(behind.title).toBe('3e1a9c0 → 8b77f2d at the remote');
		behind.run();
		expect(open).toHaveBeenCalledWith({ sha: B });
		expect(remoteUpdateAct(r, ok(A, false), open, gate).disabledReason).toBe(
			'3e1a9c0 is the newest at the remote · checked just now'
		);
	});

	it('hook and MCP entries cannot be updated in place', () => {
		const open = vi.fn();
		expect(remoteUpdateAct(rec({ kind: 'hook' }), ok(B, true), open, gate).disabledReason).toMatch(
			/hook is merged into settings.json and can't be updated in place/
		);
		expect(remoteUpdateAct(rec({ kind: 'mcp' }), ok(B, true), open, gate).disabledReason).toMatch(
			/MCP entry/
		);
	});

	it('a busy gate still blocks a behind update', () => {
		const act = remoteUpdateAct(rec(), ok(B, true), vi.fn(), () => 'busy');
		expect(act.disabledReason).toBe('busy');
	});
});
