// Tests for the pure data-derivation that drives the unified pkg surface.
//
// We test `deriveFromQueries` directly rather than mounting the hook with a
// QueryClient — the queries are just plumbing, the interesting branching
// (origin classification, update detection, screenshot ref shapes, etc.)
// lives in this pure transform.

import { describe, expect, it } from 'vitest';

import { deriveFromQueries } from './use-derived';
import type {
	PkgInstalledSummary,
	PkgManifestPreview,
	PkgPermissionViolation,
	PkgTrustEntry,
} from '@/lib/tauri-cmd';
import type { RegistryEntry } from '@/lib/registry/use-registry';

/* ───── Fixtures ───── */

function makeInstalled(
	id: string,
	overrides: Partial<PkgInstalledSummary> = {}
): PkgInstalledSummary {
	return {
		id,
		version: '0.1.0',
		ikenga_api: '1',
		install_path: `~/.config/ikenga/pkgs/${id}`,
		enabled: true,
		installed_at: 1_700_000_000,
		compatible: true,
		source: { kind: 'builtin' },
		...overrides,
	};
}

function makeManifest(overrides: Partial<PkgManifestPreview> = {}): PkgManifestPreview {
	return {
		id: 'com.test.pkg',
		name: 'Test pkg',
		version: '0.1.0',
		ikenga_api: '1',
		...overrides,
	};
}

function makeTrust(pkgId: string, overrides: Partial<PkgTrustEntry> = {}): PkgTrustEntry {
	return {
		pkg_id: pkgId,
		version: '0.1.0',
		state: 'granted',
		perms: { shell_execute: [], fs_write_outside_sandbox: [], net: [], vault_keys: [] },
		last_granted_at_ms: null,
		change_reason: null,
		auto_trusted: false,
		...overrides,
	};
}

function makeViolation(
	id: number,
	pkgId: string,
	overrides: Partial<PkgPermissionViolation> = {}
): PkgPermissionViolation {
	return {
		id,
		pkg_id: pkgId,
		scope_kind: 'shell.execute',
		attempted: 'ffmpeg',
		declared: '',
		occurred_at: 1_700_000_000,
		...overrides,
	};
}

function makeRegistryEntry(name: string, overrides: Partial<RegistryEntry> = {}): RegistryEntry {
	return {
		name,
		latest: '0.1.0',
		detail: `pkgs/${name.replace(/[@/]/g, '-')}.json`,
		...overrides,
	};
}

/* ───── Tests ───── */

describe('deriveFromQueries', () => {
	it('returns the empty shape when status data is undefined (boot)', () => {
		const d = deriveFromQueries({
			statusData: undefined,
			statusLoading: true,
		});
		expect(d.installed).toEqual([]);
		expect(d.registry).toEqual([]);
		expect(d.isLoading).toBe(true);
		expect(d.error).toBeNull();
	});

	it('surfaces the first non-null error from status / trust / violations', () => {
		expect(
			deriveFromQueries({
				statusData: undefined,
				statusError: new Error('kernel down'),
			}).error
		).toBe('kernel down');
		expect(
			deriveFromQueries({
				statusData: undefined,
				trustError: new Error('trust list failed'),
			}).error
		).toBe('trust list failed');
		expect(
			deriveFromQueries({
				statusData: undefined,
				violationsError: new Error('violations failed'),
			}).error
		).toBe('violations failed');
	});

	it('a server that does not evaluate trust is trustUnavailable, not an error and not "none"', () => {
		const reason =
			'pkg_trust_list: trust evaluation is not available on this server: no trust store';
		const d = deriveFromQueries({
			statusData: { installed: [] },
			trustError: new Error(reason),
		});
		expect(d.error).toBeNull();
		expect(d.trustUnavailable).toBe(reason);
		// Any other trust failure stays an error.
		const e = deriveFromQueries({ statusData: undefined, trustError: new Error('db locked') });
		expect(e.error).toBe('db locked');
		expect(e.trustUnavailable).toBeNull();
	});

	describe('origin classification', () => {
		it('classifies engine-kind manifests as engine regardless of source', () => {
			const d = deriveFromQueries({
				statusData: {
					installed: [
						makeInstalled('com.ikenga.engine-claude-code', { source: { kind: 'builtin' } }),
					],
				},
				manifestsData: {
					'~/.config/ikenga/pkgs/com.ikenga.engine-claude-code': makeManifest({
						kind: 'engine',
						name: 'Claude Code Engine',
					}),
				},
			});
			expect(d.engine).toHaveLength(1);
			expect(d.builtin).toHaveLength(0);
			expect(d.installed[0]?.origin).toBe('engine');
		});

		it('classifies non-engine builtin source as builtin', () => {
			const d = deriveFromQueries({
				statusData: {
					installed: [makeInstalled('com.ikenga.iyke', { source: { kind: 'builtin' } })],
				},
				manifestsData: {
					'~/.config/ikenga/pkgs/com.ikenga.iyke': makeManifest({
						kind: 'skill',
						name: 'Iyke',
					}),
				},
			});
			expect(d.builtin).toHaveLength(1);
			expect(d.user).toHaveLength(0);
		});

		it('classifies other sources as user', () => {
			const d = deriveFromQueries({
				statusData: {
					installed: [
						makeInstalled('com.royalti.storyboard', {
							source: { kind: 'local', path: '/tmp' },
						}),
					],
				},
				manifestsData: {
					'~/.config/ikenga/pkgs/com.royalti.storyboard': makeManifest({ kind: 'ui' }),
				},
			});
			expect(d.user).toHaveLength(1);
			expect(d.builtin).toHaveLength(0);
		});
	});

	describe('state derivation', () => {
		it('marks disabled pkgs regardless of sidecars', () => {
			const d = deriveFromQueries({
				statusData: {
					installed: [makeInstalled('com.test.x', { enabled: false })],
				},
				manifestsData: {
					'~/.config/ikenga/pkgs/com.test.x': makeManifest({
						sidecars: [{ name: 'worker', bin: 'bun' }],
					}),
				},
			});
			expect(d.installed[0]?.state).toBe('disabled');
		});

		it('marks running when enabled and has sidecars', () => {
			const d = deriveFromQueries({
				statusData: { installed: [makeInstalled('com.test.x')] },
				manifestsData: {
					'~/.config/ikenga/pkgs/com.test.x': makeManifest({
						sidecars: [{ name: 'worker', bin: 'bun' }],
					}),
				},
			});
			expect(d.installed[0]?.state).toBe('running');
		});

		it('marks idle when enabled and no sidecars', () => {
			const d = deriveFromQueries({
				statusData: { installed: [makeInstalled('com.test.x')] },
				manifestsData: {
					'~/.config/ikenga/pkgs/com.test.x': makeManifest({}),
				},
			});
			expect(d.installed[0]?.state).toBe('idle');
		});
	});

	describe('update detection', () => {
		const registrySource = {
			kind: 'registry',
			url: 'https://registry.npmjs.org/x.tgz',
			publisher_key: null,
		} as const;

		it('fills latest when registry has a newer version', () => {
			const d = deriveFromQueries({
				statusData: {
					installed: [makeInstalled('com.test.x', { version: '0.1.0', source: registrySource })],
				},
				registryEntries: [makeRegistryEntry('com.test.x', { latest: '0.2.0' })],
			});
			expect(d.installed[0]?.latest).toBe('0.2.0');
			expect(d.updates).toHaveLength(1);
		});

		it('leaves latest null when registry matches', () => {
			const d = deriveFromQueries({
				statusData: {
					installed: [makeInstalled('com.test.x', { version: '0.2.0', source: registrySource })],
				},
				registryEntries: [makeRegistryEntry('com.test.x', { latest: '0.2.0' })],
			});
			expect(d.installed[0]?.latest).toBeNull();
			expect(d.updates).toHaveLength(0);
		});

		it('leaves latest null when registry entry is missing', () => {
			const d = deriveFromQueries({
				statusData: { installed: [makeInstalled('com.test.x', { source: registrySource })] },
				registryEntries: [],
			});
			expect(d.installed[0]?.latest).toBeNull();
			expect(d.updates).toHaveLength(0);
		});

		it('matches across the reverse-DNS id / npm-name boundary', () => {
			// Kernel id is `com.ikenga.engine-claude-code`; registry name is
			// `@ikenga/pkg-engine-claude-code`. These never match on exact
			// equality, which is the bug that double-listed installed pkgs.
			const d = deriveFromQueries({
				statusData: {
					installed: [
						makeInstalled('com.ikenga.engine-claude-code', {
							version: '0.1.0',
							source: registrySource,
						}),
					],
				},
				registryEntries: [makeRegistryEntry('@ikenga/pkg-engine-claude-code', { latest: '0.2.0' })],
			});
			expect(d.installed[0]?.latest).toBe('0.2.0');
			expect(d.updates).toHaveLength(1);
		});

		it('never offers updates for non-registry sources (builtin / local / dev)', () => {
			// Builtins ship with the shell, and dev/local installs point at a
			// working tree — the kernel refuses a same-id install at a different
			// path, so offering these updates aborted the whole batch.
			const d = deriveFromQueries({
				statusData: {
					installed: [
						makeInstalled('com.ikenga.mcp-iyke', {
							version: '0.1.0',
							source: { kind: 'builtin' },
						}),
						makeInstalled('com.ikenga.suite', {
							version: '0.1.0',
							source: { kind: 'local', path: '/home/dev/ikenga-pkgs/packages/apps/suite' },
						}),
					],
				},
				registryEntries: [
					makeRegistryEntry('@ikenga/mcp-iyke', { latest: '0.2.1' }),
					makeRegistryEntry('@ikenga/pkg-suite', { latest: '0.3.0' }),
				],
			});
			expect(d.updates).toHaveLength(0);
			expect(d.installed.map((r) => r.latest)).toEqual([null, null]);
			// They still dedupe out of the "Available in registry" group.
			expect(d.registry).toHaveLength(0);
		});
	});

	describe('registry rows', () => {
		it('includes registry pkgs that are not installed', () => {
			const d = deriveFromQueries({
				statusData: { installed: [] },
				registryEntries: [
					makeRegistryEntry('@ikenga/mcp-browser', {
						description: 'Browser MCP',
						kind: 'mcp',
					}),
				],
			});
			expect(d.registry).toHaveLength(1);
			expect(d.registry[0]).toMatchObject({
				id: '@ikenga/mcp-browser',
				origin: 'registry',
				state: 'not-installed',
				kind: 'mcp',
				desc: 'Browser MCP',
			});
		});

		it('hides registry pkgs that are already installed', () => {
			const d = deriveFromQueries({
				statusData: { installed: [makeInstalled('com.test.x')] },
				registryEntries: [makeRegistryEntry('com.test.x')],
			});
			expect(d.registry).toHaveLength(0);
		});

		it('hides an installed pkg whose registry name uses the npm namespace', () => {
			const d = deriveFromQueries({
				statusData: { installed: [makeInstalled('com.ikenga.mcp-browser')] },
				registryEntries: [makeRegistryEntry('@ikenga/mcp-browser')],
			});
			expect(d.registry).toHaveLength(0);
		});

		it('wraps hero `screenshot` URL as a registry screenshot ref', () => {
			const d = deriveFromQueries({
				statusData: { installed: [] },
				registryEntries: [
					makeRegistryEntry('com.test.x', {
						// schema-bumped field; cast for the fixture
						...({ screenshot: 'https://cdn.example/x/hero.png' } as Partial<RegistryEntry>),
					}),
				],
			});
			const shots = d.registry[0]?.screenshots ?? [];
			expect(shots).toHaveLength(1);
			expect(shots[0]).toMatchObject({
				kind: 'url',
				src: 'https://cdn.example/x/hero.png',
			});
		});
	});

	describe('trust + violations gathering', () => {
		it('attaches trust entries by pkg_id', () => {
			const d = deriveFromQueries({
				statusData: { installed: [makeInstalled('com.test.x')] },
				trustData: [makeTrust('com.test.x', { state: 'needs_approval' })],
			});
			expect(d.installed[0]?.trust?.state).toBe('needs_approval');
			expect(d.trust).toHaveLength(1);
		});

		it('collects multiple violations per pkg', () => {
			const d = deriveFromQueries({
				statusData: { installed: [makeInstalled('com.test.x')] },
				violationsData: [
					makeViolation(1, 'com.test.x'),
					makeViolation(2, 'com.test.x', { attempted: 'rm' }),
					makeViolation(3, 'com.other'),
				],
			});
			expect(d.installed[0]?.violations).toHaveLength(2);
			expect(d.violations).toHaveLength(1); // single pkg counted once
		});
	});

	describe('screenshots on installed pkgs', () => {
		it('returns installed-pkg refs with empty src (resolved later via Tauri command)', () => {
			const d = deriveFromQueries({
				statusData: { installed: [makeInstalled('com.test.x')] },
				manifestsData: {
					'~/.config/ikenga/pkgs/com.test.x': {
						...makeManifest({}),
						screenshots: [
							{ path: 'screenshots/a.png', caption: 'first' },
							{ path: 'screenshots/b.png' },
						],
					} as PkgManifestPreview,
				},
			});
			const shots = d.installed[0]?.screenshots ?? [];
			expect(shots).toHaveLength(2);
			expect(shots[0]).toEqual({
				kind: 'installed-pkg',
				pkgId: 'com.test.x',
				path: 'screenshots/a.png',
				caption: 'first',
				src: '',
			});
			expect(shots[1]?.caption).toBeNull(); // missing caption → null
		});

		it('returns empty screenshots when no manifest is loaded', () => {
			const d = deriveFromQueries({
				statusData: { installed: [makeInstalled('com.test.x')] },
				// no manifestsData → row has no manifest
			});
			expect(d.installed[0]?.screenshots).toEqual([]);
		});

		it('skips manifest-error rows for the manifest fields but still includes the row', () => {
			const d = deriveFromQueries({
				statusData: { installed: [makeInstalled('com.test.x')] },
				manifestsData: {
					'~/.config/ikenga/pkgs/com.test.x': { _error: 'parse failed' },
				},
			});
			expect(d.installed).toHaveLength(1);
			expect(d.installed[0]?.manifest).toBeNull();
			expect(d.installed[0]?.screenshots).toEqual([]);
			// Falls back to the install id when the manifest can't name it
			expect(d.installed[0]?.name).toBe('com.test.x');
		});
	});

	describe('sidecar counting', () => {
		it('sums sidecars across running pkgs only', () => {
			const d = deriveFromQueries({
				statusData: {
					installed: [
						makeInstalled('a'),
						makeInstalled('b'),
						makeInstalled('c', { enabled: false }),
					],
				},
				manifestsData: {
					'~/.config/ikenga/pkgs/a': makeManifest({
						sidecars: [
							{ name: 's1', bin: 'bun' },
							{ name: 's2', bin: 'bun' },
						],
					}),
					'~/.config/ikenga/pkgs/b': makeManifest({
						sidecars: [{ name: 's3', bin: 'bun' }],
					}),
					'~/.config/ikenga/pkgs/c': makeManifest({
						// Disabled — should NOT count even though it declares a sidecar.
						sidecars: [{ name: 's4', bin: 'bun' }],
					}),
				},
			});
			expect(d.sidecarsRunning).toBe(3);
		});
	});

	describe('summarizeScopes', () => {
		it('expands array-valued perms into namespace:value strings', () => {
			const d = deriveFromQueries({
				statusData: { installed: [makeInstalled('com.test.x')] },
				manifestsData: {
					'~/.config/ikenga/pkgs/com.test.x': makeManifest({
						permissions: {
							'fs:read': ['workspace', 'workspace/storyboards'],
							'shell.execute': ['claude'],
						},
					}),
				},
			});
			const scopes = d.installed[0]?.scopes ?? [];
			expect(scopes).toContain('fs:read:workspace');
			expect(scopes).toContain('fs:read:workspace/storyboards');
			expect(scopes).toContain('shell.execute:claude');
		});

		it('keeps boolean-true perms as bare namespace', () => {
			const d = deriveFromQueries({
				statusData: { installed: [makeInstalled('com.test.x')] },
				manifestsData: {
					'~/.config/ikenga/pkgs/com.test.x': makeManifest({
						permissions: { 'shell:engine:default': true },
					}),
				},
			});
			expect(d.installed[0]?.scopes).toContain('shell:engine:default');
		});

		it('skips empty arrays', () => {
			const d = deriveFromQueries({
				statusData: { installed: [makeInstalled('com.test.x')] },
				manifestsData: {
					'~/.config/ikenga/pkgs/com.test.x': makeManifest({
						permissions: { 'fs:read': [] },
					}),
				},
			});
			expect(d.installed[0]?.scopes).toEqual([]);
		});
	});
});
