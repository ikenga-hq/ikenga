import { describe, expect, it } from 'vitest';

import type { ClaudeStoreEntry, RequiresEntry } from '@/lib/tauri-cmd';
import {
	catalogGitRef,
	catalogPin,
	catalogPinMoved,
	catalogPins,
	catalogRefs,
	isPinned,
	mergePrimitiveView,
	parseCatalog,
	resolveCatalogClosure,
	shaMatches,
	shortSha,
	type PrimitiveCatalogEntry,
} from './primitives';

// ── fixtures ────────────────────────────────────────────────────────────────

function cat(
	name: string,
	requires?: RequiresEntry[],
	over?: Partial<PrimitiveCatalogEntry>
): PrimitiveCatalogEntry {
	return {
		kind: 'skill',
		name,
		version: '0.1.0',
		description: null,
		source: 'npx',
		url: `ikenga-hq/${name}`,
		publisher: 'royalti-io',
		...(requires ? { requires } : {}),
		...over,
	};
}

const ref = (name: string, extra?: Partial<RequiresEntry>): RequiresEntry => ({
	kind: 'skill',
	name,
	...extra,
});

const installed = (names: string[]): Set<string> => new Set(names.map((n) => `skill:${n}`));

// resolveCatalogClosure only reads kind+name off store entries via installedKeys,
// so we never need a full ClaudeStoreEntry here — the Set is the contract.
const _storeShape: ClaudeStoreEntry[] = []; // type-only anchor
void _storeShape;

describe('resolveCatalogClosure (WP-15 consent surface)', () => {
	it('returns an empty closure for a dep-free primitive', () => {
		const t = cat('artifact-builder');
		expect(resolveCatalogClosure(t, [t], new Set())).toEqual([]);
	});

	it('resolves a catalogued dep with inherited trust (no extra confirm)', () => {
		const dep = cat('design-language');
		const t = cat('artifact-builder', [ref('design-language')]);
		const out = resolveCatalogClosure(t, [t, dep], new Set());
		expect(out).toHaveLength(1);
		expect(out[0]).toMatchObject({
			name: 'design-language',
			resolution: 'catalog',
			satisfied: false,
			needsExtraConfirm: false,
			provenance: 'npx · ikenga-hq/design-language',
		});
	});

	it('flags a self-pinned non-catalog dep as needing an extra confirm', () => {
		const t = cat('app', [ref('skill-core', { source: 'git', ref: 'v2' })]);
		const out = resolveCatalogClosure(t, [t], new Set());
		expect(out[0]).toMatchObject({
			name: 'skill-core',
			resolution: 'pinned',
			needsExtraConfirm: true,
			provenance: 'git @ v2 · not in catalog',
		});
	});

	it('flags an un-pinned non-catalog dep as unresolved + extra confirm', () => {
		const t = cat('app', [ref('mystery')]);
		const out = resolveCatalogClosure(t, [t], new Set());
		expect(out[0]).toMatchObject({
			name: 'mystery',
			resolution: 'unresolved',
			needsExtraConfirm: true,
		});
	});

	it('marks an already-installed dep satisfied (listed, not re-installed)', () => {
		const dep = cat('design-language');
		const t = cat('artifact-builder', [ref('design-language')]);
		const out = resolveCatalogClosure(t, [t, dep], installed(['design-language']));
		expect(out[0]).toMatchObject({ name: 'design-language', satisfied: true });
	});

	it('walks the transitive closure through catalogued parents', () => {
		const c = cat('c');
		const b = cat('b', [ref('c')]);
		const a = cat('a', [ref('b')]);
		const t = cat('app', [ref('a')]);
		const out = resolveCatalogClosure(t, [t, a, b, c], new Set());
		expect(out.map((d) => d.name)).toEqual(['a', 'b', 'c']);
	});

	it('dedupes a diamond dependency', () => {
		const d = cat('d');
		const b = cat('b', [ref('d')]);
		const c = cat('c', [ref('d')]);
		const t = cat('app', [ref('b'), ref('c')]);
		const out = resolveCatalogClosure(t, [t, b, c, d], new Set());
		expect(out.map((d) => d.name)).toEqual(['b', 'c', 'd']);
	});

	it('does not hang on a dependency cycle', () => {
		const a = cat('a', [ref('b')]);
		const b = cat('b', [ref('a')]);
		const t = cat('app', [ref('a')]);
		const out = resolveCatalogClosure(t, [t, a, b], new Set());
		expect(out.map((d) => d.name).sort()).toEqual(['a', 'b']);
	});
});

// ── R57 · Q3 — pinned catalog entries ─────────────────────────────────────────

const SHA = '9c41e07a1b2c3d4e5f60718293a4b5c6d7e8f901';
const HASH = `sha256-${'ab'.repeat(32)}`;

function raw(extra: Record<string, unknown> = {}) {
	return {
		kind: 'skill',
		name: 'groundwork',
		version: '0.1.0',
		source: 'npx',
		url: 'royalti-io/groundwork',
		...extra,
	};
}

function store(over: Partial<ClaudeStoreEntry> = {}): ClaudeStoreEntry {
	return {
		kind: 'skill',
		name: 'groundwork',
		storePath: '/vault/skills/groundwork',
		description: null,
		modifiedMs: 0,
		enabledIn: [],
		...over,
	};
}

describe('parseCatalog — R57 pins (ref / hash)', () => {
	it('parses an entry without a pin exactly as before (unpinned)', () => {
		const [e] = parseCatalog({ primitives: [raw()] });
		expect(e.ref).toBeUndefined();
		expect(e.hash).toBeUndefined();
		expect(isPinned(e)).toBe(false);
		expect(catalogPin(e)).toBeNull();
	});

	it('keeps a SHA ref and a sha256 content hash', () => {
		const [e] = parseCatalog({ primitives: [raw({ ref: SHA, hash: HASH })] });
		expect(e.ref).toBe(SHA);
		expect(e.hash).toBe(HASH);
		expect(catalogPin(e)).toEqual({ sha: SHA, hash: HASH });
		expect(catalogGitRef(e)).toBeNull();
	});

	it('reads a non-hex ref as a tag to fetch, not a SHA pin', () => {
		const [e] = parseCatalog({ primitives: [raw({ ref: 'v1.2.0' })] });
		expect(catalogPin(e)).toBeNull();
		expect(catalogGitRef(e)).toBe('v1.2.0');
		const [h] = parseCatalog({ primitives: [raw({ ref: 'v1.2.0', hash: HASH })] });
		expect(catalogPin(h)).toEqual({ sha: null, hash: HASH });
	});

	it('rejects a malformed hash instead of reading it as unpinned', () => {
		expect(() => parseCatalog({ primitives: [raw({ hash: 'sha256-xyz' })] })).toThrow(
			/invalid hash/
		);
		expect(() => parseCatalog({ primitives: [raw({ hash: `md5-${'a'.repeat(32)}` })] })).toThrow(
			/invalid hash/
		);
		expect(() => parseCatalog({ primitives: [raw({ hash: 42 })] })).toThrow(/invalid hash/);
	});

	it('rejects a non-string or blank ref', () => {
		expect(() => parseCatalog({ primitives: [raw({ ref: 7 })] })).toThrow(/invalid ref/);
		expect(() => parseCatalog({ primitives: [raw({ ref: ' ' })] })).toThrow(/invalid ref/);
	});

	it('carries pins into the install snapshot and the auto-update pins', () => {
		const entries = parseCatalog({
			primitives: [raw({ ref: SHA, hash: HASH }), raw({ name: 'impeccable' })],
		});
		expect(catalogRefs(entries)).toEqual([
			{
				kind: 'skill',
				name: 'groundwork',
				source: 'npx',
				url: 'royalti-io/groundwork',
				ref: SHA,
				hash: HASH,
			},
			{ kind: 'skill', name: 'impeccable', source: 'npx', url: 'royalti-io/groundwork' },
		]);
		expect(catalogPins(entries)).toEqual([
			{ kind: 'skill', name: 'groundwork', sha: SHA, hash: HASH },
		]);
	});
});

describe('pin helpers', () => {
	it('shaMatches compares prefixes either way, case-insensitively', () => {
		expect(shaMatches(SHA, '9C41E07')).toBe(true);
		expect(shaMatches('9c41e07', SHA)).toBe(true);
		expect(shaMatches(SHA, '8b77f2d')).toBe(false);
		expect(shaMatches(null, SHA)).toBe(false);
	});

	it('shortSha shortens SHAs only', () => {
		expect(shortSha(SHA)).toBe('9c41e07');
		expect(shortSha('0.1.0')).toBe('0.1.0');
		expect(shortSha(null)).toBe('—');
	});

	it('catalogPinMoved: behind when the SHA or the hash differs', () => {
		const [pinned] = parseCatalog({ primitives: [raw({ ref: SHA, hash: HASH })] });
		expect(catalogPinMoved(store({ version: SHA, hash: HASH }), pinned)).toBe(false);
		expect(catalogPinMoved(store({ version: SHA.slice(0, 7), hash: HASH }), pinned)).toBe(false);
		expect(catalogPinMoved(store({ version: '3e1a9c0aaaa', hash: HASH }), pinned)).toBe(true);
		expect(
			catalogPinMoved(store({ version: SHA, hash: `sha256-${'cd'.repeat(32)}` }), pinned)
		).toBe(true);
		// A pre-R57 record (no hash) at the pinned SHA is current.
		expect(catalogPinMoved(store({ version: SHA }), pinned)).toBe(false);
		const [hashOnly] = parseCatalog({ primitives: [raw({ hash: HASH })] });
		expect(catalogPinMoved(store({ version: SHA }), hashOnly)).toBe(true);
		expect(catalogPinMoved(store({ version: SHA, hash: HASH }), hashOnly)).toBe(false);
		const [unpinned] = parseCatalog({ primitives: [raw()] });
		expect(catalogPinMoved(store({ version: 'anything' }), unpinned)).toBe(false);
	});
});

describe('mergePrimitiveView — updatability with pins', () => {
	const [pinned] = parseCatalog({ primitives: [raw({ ref: SHA, hash: HASH })] });

	it('a catalog install behind its pin is updatable', () => {
		const [row] = mergePrimitiveView(
			[store({ version: '3e1a9c0ffff', fromCatalog: true })],
			[pinned]
		);
		expect(row.status).toBe('updatable');
	});

	it('a catalog install at its pin is installed', () => {
		const [row] = mergePrimitiveView(
			[store({ version: SHA, hash: HASH, fromCatalog: true })],
			[pinned]
		);
		expect(row.status).toBe('installed');
	});

	it('a same-named direct install never follows the catalog pin', () => {
		const [row] = mergePrimitiveView(
			[store({ version: '3e1a9c0ffff', fromCatalog: false })],
			[pinned]
		);
		expect(row.status).toBe('installed');
	});

	it('unpinned entries keep the semver rule, and a SHA version is never "behind"', () => {
		const [unpinned] = parseCatalog({ primitives: [raw({ version: '0.2.0' })] });
		expect(mergePrimitiveView([store({ version: '0.1.0' })], [unpinned])[0].status).toBe(
			'updatable'
		);
		expect(mergePrimitiveView([store({ version: SHA })], [unpinned])[0].status).toBe('installed');
		expect(mergePrimitiveView([store({ version: 'abc1234def' })], [unpinned])[0].status).toBe(
			'installed'
		);
	});
});
