import type { FetchedIndex, RegistryEntry } from '@ikenga/registry-client';
import { describe, expect, it } from 'vitest';

import { isHiddenRegistryEntry, withVerifiedVisibility } from './visibility';

const entry = (name: string): RegistryEntry => ({
	name,
	latest: '1.0.0',
	detail: `pkgs/${name}.json`,
});

/** A fetched index the way `@ikenga/registry-client` returns it: the parsed
 *  entries have lost `visibility`, the verified raw bytes still carry it. */
function fetched(rawPkgs: unknown[], parsedNames: string[]): FetchedIndex {
	const raw = new TextEncoder().encode(
		JSON.stringify({ $schemaVersion: 1, updatedAt: '2026-10-03T00:00:00.000Z', pkgs: rawPkgs })
	);
	return {
		index: {
			$schemaVersion: 1,
			updatedAt: '2026-10-03T00:00:00.000Z',
			pkgs: parsedNames.map(entry),
		},
		raw,
		signature: 'sig',
		indexUrl: 'https://registry.example/index.json',
	} as FetchedIndex;
}

describe('isHiddenRegistryEntry', () => {
	it('is true only for visibility "hidden"', () => {
		expect(isHiddenRegistryEntry({ ...entry('a'), visibility: 'hidden' } as RegistryEntry)).toBe(
			true
		);
		expect(isHiddenRegistryEntry({ ...entry('a'), visibility: 'public' } as RegistryEntry)).toBe(
			false
		);
		expect(isHiddenRegistryEntry(entry('a'))).toBe(false);
	});
});

describe('withVerifiedVisibility', () => {
	it('puts visibility "hidden" back on the entries the signed index marks hidden', () => {
		const out = withVerifiedVisibility(
			fetched(
				[
					{ name: '@ikenga/pkg-tasks', latest: '1.0.0', detail: 'x' },
					{ name: '@ikenga/pkg-finance', latest: '1.0.0', detail: 'x', visibility: 'hidden' },
				],
				['@ikenga/pkg-tasks', '@ikenga/pkg-finance']
			)
		);
		const byName = new Map(out.index.pkgs.map((p) => [p.name, p]));
		expect(isHiddenRegistryEntry(byName.get('@ikenga/pkg-finance') as RegistryEntry)).toBe(true);
		expect(isHiddenRegistryEntry(byName.get('@ikenga/pkg-tasks') as RegistryEntry)).toBe(false);
		expect(out.index.pkgs).toHaveLength(2);
	});

	it('keeps every entry, so hidden pkgs still resolve by exact name', () => {
		const out = withVerifiedVisibility(
			fetched(
				[{ name: '@ikenga/pkg-mail', latest: '1.0.0', detail: 'x', visibility: 'hidden' }],
				['@ikenga/pkg-mail']
			)
		);
		expect(out.index.pkgs.map((p) => p.name)).toEqual(['@ikenga/pkg-mail']);
		expect(out.index.pkgs[0]?.latest).toBe('1.0.0');
	});

	it('returns the same object when nothing is hidden', () => {
		const input = fetched([{ name: 'a', latest: '1.0.0', detail: 'x' }], ['a']);
		expect(withVerifiedVisibility(input)).toBe(input);
	});

	it('ignores a "hidden" flag on a name the parsed index does not have', () => {
		const out = withVerifiedVisibility(
			fetched([{ name: 'ghost', latest: '1.0.0', detail: 'x', visibility: 'hidden' }], ['a'])
		);
		expect(out.index.pkgs.map((p) => p.name)).toEqual(['a']);
		expect(isHiddenRegistryEntry(out.index.pkgs[0] as RegistryEntry)).toBe(false);
	});

	it('fails open when the raw bytes cannot be read', () => {
		const input = fetched([], ['a']);
		input.raw = new TextEncoder().encode('not json');
		expect(withVerifiedVisibility(input)).toBe(input);
	});

	it('does not touch the signature, url or timestamps', () => {
		const input = fetched(
			[{ name: 'a', latest: '1.0.0', detail: 'x', visibility: 'hidden' }],
			['a']
		);
		const out = withVerifiedVisibility(input);
		expect(out.signature).toBe('sig');
		expect(out.indexUrl).toBe('https://registry.example/index.json');
		expect(out.raw).toBe(input.raw);
		expect(out.index.updatedAt).toBe('2026-10-03T00:00:00.000Z');
	});
});
