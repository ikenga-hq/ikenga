// WP-61 unit tests (written under DEC-50, not run this session). Exercises
// `buildPackageDiff` against fabricated `PackageKeyRequest`s and the
// DEFAULT-only keymap `keyHolder()` reads without a store harness (see
// `vscode.test.ts`'s header note).

import { describe, expect, it } from 'vitest';
import type { EffectiveModel, PackageKeyRequest } from '@/lib/actions/store';
import { buildPackageDiff } from './package';

function fakeModel(requests: PackageKeyRequest[], names: Record<string, string> = {}): EffectiveModel {
	return {
		actionById: {
			get: (id: string) => (names[id] ? ({ name: names[id] } as never) : undefined),
		},
		keymap: { packageRequests: requests } as never,
	} as unknown as EffectiveModel;
}

function request(over: Partial<PackageKeyRequest>): PackageKeyRequest {
	return {
		actionId: 'com.ikenga.git:stage-file',
		pkgId: 'com.ikenga.git',
		key: 'mod+alt+shift+f18',
		when: 'filesFocus',
		origin: 'context_action',
		byPlatform: { mac: { status: 'granted' }, other: { status: 'granted' } },
		...over,
	};
}

describe('buildPackageDiff', () => {
	it('adds a request whose key is free, targeting the personal file by default', () => {
		const model = fakeModel([request({})], { 'com.ikenga.git:stage-file': 'Stage file' });
		const [row] = buildPackageDiff(model);
		expect(row.kind).toBe('add');
		expect(row.title).toBe('Stage file');
		expect(row.write).toEqual({ key: 'mod+alt+shift+f18', command: 'com.ikenga.git:stage-file', when: 'filesFocus' });
	});

	it('skips a request already held by its own action id (the common, already-granted case)', () => {
		// `explorer.toggle`'s own default key, requested by an action of the
		// same id — a scenario that can't happen for a *real* package (ids are
		// namespaced), used here only to land on a key the default keymap
		// definitely holds without depending on package-grant merge logic.
		const model = fakeModel([request({ actionId: 'explorer.toggle', key: 'mod+b' })]);
		const [row] = buildPackageDiff(model);
		expect(row.kind).toBe('skip');
	});

	it('reports a clash when a different command holds the key', () => {
		const model = fakeModel([request({ key: 'mod+b' })]); // held by `explorer.toggle`, not this request's own id
		const [row] = buildPackageDiff(model);
		expect(row.kind).toBe('clash');
		expect(row.detail).toMatch(/explorer\.toggle/);
	});

	it('falls back to the bare action id when the model has no name for it', () => {
		const model = fakeModel([request({})]);
		const [row] = buildPackageDiff(model);
		expect(row.title).toBe('com.ikenga.git:stage-file');
	});

	it('skips an invalid request without ever querying keyHolder on a malformed key', () => {
		const model = fakeModel([
			request({
				key: 'ctrl+k ctrl+r ctrl+x', // a package request can never be a chord (§7.1)
				byPlatform: {
					mac: { status: 'invalid', reason: 'a package key request is a single stroke (B-6)' },
					other: { status: 'invalid', reason: 'a package key request is a single stroke (B-6)' },
				},
			}),
		]);
		const [row] = buildPackageDiff(model);
		expect(row.kind).toBe('skip');
		expect(row.detail).toMatch(/single stroke/);
	});
});
