// Store install-sheet derivations (WP-15 / locked D-02).

import { describe, expect, it } from 'vitest';
import {
	asksLabel,
	closureLabel,
	consentGroups,
	formatBytes,
	type StoreManifest,
} from './store-detail';

function manifest(partial: Record<string, unknown>): StoreManifest {
	return {
		id: 'com.ikenga.x',
		name: 'x',
		version: '1.0.0',
		ikenga_api: '1',
		mcp: [],
		sidecars: [],
		requires: [],
		permissions: {},
		...partial,
	} as unknown as StoreManifest;
}

// Mirrors the published `pkgs/studio.json` latest manifest.
const studio = manifest({
	requires: [
		{ kind: 'bundle', name: 'studio-archetypes', source: 'npx' },
		{ kind: 'bundle', name: 'studio-toolchain', source: 'npx' },
		{ kind: 'skill', name: 'studio-beat-detect', source: 'npx' },
		{ kind: 'skill', name: 'studio-doctor', source: 'npx' },
		{ kind: 'skill', name: 'video-script-structure', source: 'npx' },
		{ kind: 'skill', name: 'storyboard-workflow', source: 'npx' },
	],
	mcp: [{ name: 'studio' }],
	sidecars: [{ name: 'pa-com-ikenga-studio-project' }],
	permissions: {
		'shell.execute': ['bun', 'npx', 'node', 'ffmpeg', 'chromium', 'python3'],
		'fs.read': ['$pkg_data/**'],
		'fs.write': ['$pkg_data/**'],
		net: [
			'http://127.0.0.1:*',
			'https://esm.sh',
			'https://cdn.jsdelivr.net',
			'https://cdn.tailwindcss.com',
			'https://*.supabase.co',
		],
		'vault.keys': ['studio.veo', 'studio.kling', 'studio.runway'],
	},
});

describe('closureLabel', () => {
	it('says "not read" without a manifest', () => {
		expect(closureLabel('app', null)).toBe('closure not read');
		expect(closureLabel('bundle', null)).toBe('bundle, published as one unit');
	});

	it('counts requires, then the pkg’s own sidecars and MCP servers', () => {
		expect(closureLabel('app', studio)).toBe(
			'also installs 2 bundles · 4 skills · 1 sidecar · 1 MCP server'
		);
	});

	it('lists own parts without "also installs" when nothing is required', () => {
		expect(
			closureLabel('app', manifest({ mcp: [{ name: 'git' }], sidecars: [{ name: 'repo' }] }))
		).toBe('1 sidecar · 1 MCP server');
		expect(closureLabel('skill', manifest({}))).toBe('no requires');
	});

	it('describes a bundle as one published unit', () => {
		const bundle = manifest({
			requires: [
				{ kind: 'skill', name: 'a' },
				{ kind: 'skill', name: 'b' },
			],
		});
		expect(closureLabel('bundle', bundle)).toBe('bundle · 2 skills, published as one unit');
		expect(closureLabel('bundle', manifest({}))).toBe('bundle, published as one unit');
	});
});

describe('asksLabel', () => {
	it('says "permissions not read" without a manifest', () => {
		expect(asksLabel('app', null)).toBe('permissions not read');
	});

	it('summarises the D-02 studio asks', () => {
		expect(asksLabel('app', studio)).toBe(
			'asks: shell.execute · fs.write $pkg_data · net (5 hosts) · vault.keys (3)'
		);
	});

	it('distinguishes declarative kinds from pkgs that simply ask for nothing', () => {
		expect(asksLabel('skill', manifest({}))).toBe('asks: declares intent, never grants');
		expect(asksLabel('engine', manifest({}))).toBe('asks: nothing');
	});
});

describe('consentGroups', () => {
	it('emits one consent per permission group, merging identical fs read/write', () => {
		const groups = consentGroups(studio);
		expect(groups.map((g) => g.label)).toEqual([
			'shell.execute',
			'net',
			'fs.read · fs.write',
			'vault.keys',
		]);
		expect(groups[0].detail).toBe(
			'bun · npx · node · ffmpeg · chromium · python3 — runs these binaries on your machine.'
		);
		expect(groups[1].detail).toBe(
			'127.0.0.1:* · esm.sh · cdn.jsdelivr.net · cdn.tailwindcss.com · *.supabase.co'
		);
		expect(groups[2].detail).toBe('$pkg_data/** only — inside its own folder.');
	});

	it('keeps differing fs scopes as separate consents', () => {
		const groups = consentGroups(
			manifest({ permissions: { 'fs.read': ['$home/**'], 'fs.write': ['$pkg_data/**'] } })
		);
		expect(groups.map((g) => g.id)).toEqual(['fs.read', 'fs.write']);
	});

	it('is empty when the manifest asks for nothing', () => {
		expect(consentGroups(manifest({}))).toEqual([]);
		expect(consentGroups(manifest({ permissions: { 'shell.execute': [], net: [] } }))).toEqual([]);
	});
});

describe('formatBytes', () => {
	it('formats tarball sizes', () => {
		expect(formatBytes(undefined)).toBeNull();
		expect(formatBytes(512)).toBe('512 B');
		expect(formatBytes(133_549)).toBe('130 KB');
		expect(formatBytes(2_337_306)).toBe('2.2 MB');
		expect(formatBytes(25_327_224)).toBe('24 MB');
	});
});
