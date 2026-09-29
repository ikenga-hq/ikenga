// useStoreInstall — R57 vault path (git / npx primitives). Pins: every fetch
// carries the reviewed pin; placing is `claudePrimitiveEnable` for the deps the
// fetch materialized (enable order) and the target, in the chosen Ọba scope;
// Q5 enables an already-satisfied dep that the chosen scope can't reach; a
// refused (pin-mismatch) fetch places nothing.

import { beforeEach, describe, expect, it, vi } from 'vitest';
import { renderHook } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

const oba = vi.hoisted(() => ({
	installWithDeps: vi.fn(),
	installGit: vi.fn(),
	installNpx: vi.fn(),
	update: vi.fn(),
	enable: vi.fn(),
}));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	obaInstallWithDeps: (...args: unknown[]) => oba.installWithDeps(...args),
	obaInstallGit: (...args: unknown[]) => oba.installGit(...args),
	obaInstallNpx: (...args: unknown[]) => oba.installNpx(...args),
	obaUpdate: (...args: unknown[]) => oba.update(...args),
	claudePrimitiveEnable: (...args: unknown[]) => oba.enable(...args),
}));

vi.mock('@/lib/registry/use-registry', async (orig) => ({
	...(await orig<typeof import('@/lib/registry/use-registry')>()),
	useRegistryIndex: () => ({ data: { indexUrl: 'https://registry.test/index.json' } }),
}));

import { useShellStore } from '@/lib/shell/shell-store';
import type { PrimitiveCatalogEntry } from '@/lib/registry/primitives';
import type { ClaudeStoreEntry, ResolvedSource } from '@/lib/tauri-cmd';
import type { NgwaCatalogRow } from './enrichment';
import { placeInstalled, primitiveScopeWire, useStoreInstall } from './use-store-install';

const SHA = '9c41e07a1b2c3d4e5f60718293a4b5c6d7e8f901';
const HASH = `sha256-${'ab'.repeat(32)}`;

function catEntry(name: string, over: Partial<PrimitiveCatalogEntry> = {}): PrimitiveCatalogEntry {
	return {
		kind: 'skill',
		name,
		version: '0.1.0',
		description: null,
		source: 'npx',
		url: `royalti-io/${name}`,
		...over,
	};
}
function row(e: PrimitiveCatalogEntry): NgwaCatalogRow {
	return {
		id: `cat:${e.kind}:${e.name}`,
		name: e.name,
		kind: e.kind as NgwaCatalogRow['kind'],
		storeKind: e.kind,
		source: e.source,
		url: e.url,
		version: e.version,
		description: null,
		publisher: null,
		entry: e,
		installed: null,
		isUpdate: false,
	};
}
function vaulted(name: string, enabledIn: ClaudeStoreEntry['enabledIn']): ClaudeStoreEntry {
	return {
		kind: 'skill',
		name,
		storePath: `/v/${name}`,
		description: null,
		modifiedMs: 0,
		enabledIn,
	};
}
const entryOf = (name: string) => vaulted(name, []);

function setup() {
	const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	const invalidate = vi.spyOn(qc, 'invalidateQueries');
	const wrapper = ({ children }: { children: ReactNode }) => (
		<QueryClientProvider client={qc}>{children}</QueryClientProvider>
	);
	const { result } = renderHook(() => useStoreInstall(), { wrapper });
	return { hook: result.current, invalidate };
}
const invalidatedKeys = (spy: { mock: { calls: unknown[][] } }) =>
	spy.mock.calls.map((c) => (c[0] as { queryKey: unknown[] }).queryKey);

const resolved = (over: Partial<ResolvedSource> = {}): ResolvedSource => ({
	kind: 'skill',
	name: 'liner-notes',
	inferredFrom: 'root SKILL.md',
	source: 'git',
	url: 'https://github.com/kolanut-labs/liner-notes',
	ref: null,
	sha: SHA,
	hash: HASH,
	files: ['SKILL.md'],
	requires: [],
	description: null,
	trust: 'unsigned',
	...over,
});

beforeEach(() => {
	for (const f of Object.values(oba)) f.mockReset();
	oba.enable.mockResolvedValue({});
	oba.installGit.mockResolvedValue(entryOf('x'));
	oba.installNpx.mockResolvedValue(entryOf('x'));
	oba.update.mockResolvedValue(entryOf('x'));
	useShellStore.setState({ activeProjectId: 'royalti-co' });
});

describe('R57 · primitive installs', () => {
	it('maps primitive scopes: personal → workspace, project → project:<id>', () => {
		expect(primitiveScopeWire('personal', 'p1')).toBe('workspace');
		expect(primitiveScopeWire('project', 'p1')).toBe('project:p1');
		expect(() => primitiveScopeWire('project', null)).toThrow(/No active project/);
	});

	it('installs a catalog row pinned, places target + deps, and enables an unreachable satisfied dep (Q5)', async () => {
		const target = catEntry('ikenga-artifact-builder', {
			ref: SHA,
			hash: HASH,
			requires: [
				{ kind: 'skill', name: 'design-language' },
				{ kind: 'skill', name: 'groundwork' },
				{ kind: 'skill', name: 'impeccable' },
			],
		});
		const dep = catEntry('design-language', { ref: 'v2' });
		oba.installWithDeps.mockResolvedValue({
			target: entryOf('ikenga-artifact-builder'),
			installed: [entryOf('design-language')],
			alreadySatisfied: [
				{ kind: 'skill', name: 'groundwork' },
				{ kind: 'skill', name: 'impeccable' },
			],
		});
		const stages: string[] = [];
		const { hook, invalidate } = setup();
		const out = await hook.installPrimitive(row(target), 'project', {
			catalog: [target, dep],
			// groundwork is enabled only in another project → Q5 enables it here;
			// impeccable is personal → reachable from every project, left alone.
			vault: [vaulted('groundwork', ['project:elsewhere']), vaulted('impeccable', ['workspace'])],
			onStage: (st) => stages.push(st),
		});

		expect(oba.installWithDeps).toHaveBeenCalledWith(
			'skill',
			'ikenga-artifact-builder',
			'npx',
			'royalti-io/ikenga-artifact-builder',
			[
				expect.objectContaining({ name: 'ikenga-artifact-builder', ref: SHA, hash: HASH }),
				expect.objectContaining({ name: 'design-language', ref: 'v2' }),
			],
			null,
			true,
			{ sha: SHA, hash: HASH }
		);
		expect(oba.enable.mock.calls).toEqual([
			['skill', 'design-language', 'project:royalti-co'],
			['skill', 'ikenga-artifact-builder', 'project:royalti-co'],
			['skill', 'groundwork', 'project:royalti-co'],
		]);
		expect(out.placed.map((p) => p.name)).toEqual(['ikenga-artifact-builder', 'design-language']);
		expect(out.alsoEnabled.map((p) => p.name)).toEqual(['groundwork']);
		expect(out.leftInPlace.map((p) => p.name)).toEqual(['impeccable']);
		expect(stages).toEqual(['fetch', 'place']);
		expect(invalidatedKeys(invalidate)).toContainEqual(['claude_store']);
		expect(invalidatedKeys(invalidate)).toContainEqual(['ngwa', 'snapshot']);
	});

	it('an unpinned tag ref is fetched as the git ref, with no pin', async () => {
		oba.installWithDeps.mockResolvedValue({
			target: entryOf('a'),
			installed: [],
			alreadySatisfied: [],
		});
		const { hook } = setup();
		await hook.installPrimitive(row(catEntry('a', { source: 'git', ref: 'v1.2.0' })), 'personal', {
			catalog: [],
			vault: [],
		});
		const call = oba.installWithDeps.mock.calls[0];
		expect(call[5]).toBe('v1.2.0');
		expect(call[7]).toBeNull();
		expect(oba.enable).toHaveBeenCalledWith('skill', 'a', 'workspace');
	});

	it('placeInstalled: a dep already enabled in the chosen scope is reachable, not re-enabled', async () => {
		const out = await placeInstalled({
			target: { kind: 'skill', name: 't' },
			installed: [],
			satisfied: [{ kind: 'skill', name: 'd' }],
			scope: 'project:p1',
			vault: [vaulted('d', ['project:p1'])],
		});
		expect(oba.enable.mock.calls).toEqual([['skill', 't', 'project:p1']]);
		expect(out.leftInPlace).toEqual([{ kind: 'skill', name: 'd' }]);
	});

	it('places nothing when the pinned fetch is refused (pin mismatch)', async () => {
		oba.installWithDeps.mockRejectedValue(new Error('pin mismatch: source serves 8b77f2d'));
		const { hook } = setup();
		await expect(
			hook.installPrimitive(row(catEntry('a', { ref: SHA })), 'personal', {
				catalog: [],
				vault: [],
			})
		).rejects.toThrow('pin mismatch');
		expect(oba.enable).not.toHaveBeenCalled();
	});

	it('updatePrimitive moves to the catalog pin, and refuses an unpinned row', async () => {
		const { hook } = setup();
		await hook.updatePrimitive(row(catEntry('a', { ref: SHA, hash: HASH })));
		expect(oba.update).toHaveBeenCalledWith('skill', 'a', { sha: SHA, hash: HASH });
		await expect(hook.updatePrimitive(row(catEntry('b')))).rejects.toThrow(/no catalog pin/);
	});

	it('installResolved: a dep-free git source installs direct, pinned to the resolve', async () => {
		const { hook } = setup();
		await hook.installResolved(resolved({ kind: 'agent', ref: 'main' }), 'personal', {
			catalog: [],
			vault: [],
		});
		expect(oba.installGit).toHaveBeenCalledWith(
			'agent',
			'liner-notes',
			'https://github.com/kolanut-labs/liner-notes',
			'main',
			false,
			{ sha: SHA, hash: HASH }
		);
		expect(oba.enable).toHaveBeenCalledWith('agent', 'liner-notes', 'workspace');
	});

	it('installResolved: an npx spec installs as a skill', async () => {
		const { hook } = setup();
		await hook.installResolved(
			resolved({ source: 'npx', url: 'kolanut-labs/liner-notes', sha: null }),
			'project',
			{ catalog: [], vault: [] }
		);
		expect(oba.installNpx).toHaveBeenCalledWith(
			'skill',
			'liner-notes',
			'kolanut-labs/liner-notes',
			false,
			{ sha: null, hash: HASH }
		);
		expect(oba.enable).toHaveBeenCalledWith('skill', 'liner-notes', 'project:royalti-co');
	});

	it('installResolved: a source with requires goes through the resolver, fromCatalog=false', async () => {
		oba.installWithDeps.mockResolvedValue({
			target: entryOf('liner-notes'),
			installed: [],
			alreadySatisfied: [],
		});
		const { hook } = setup();
		await hook.installResolved(
			resolved({ requires: [{ kind: 'skill', name: 'credits-parse', source: 'git', ref: 'v2' }] }),
			'project',
			{ catalog: [catEntry('design-language')], vault: [] }
		);
		expect(oba.installWithDeps).toHaveBeenCalledWith(
			'skill',
			'liner-notes',
			'git',
			'https://github.com/kolanut-labs/liner-notes',
			[expect.objectContaining({ name: 'design-language' })],
			null,
			false,
			{ sha: SHA, hash: HASH }
		);
		expect(oba.installGit).not.toHaveBeenCalled();
	});
});
