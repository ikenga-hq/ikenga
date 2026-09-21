import { describe, expect, it } from 'vitest';
import {
	type NgwaItem,
	type NgwaSnapshot,
	NgwaSnapshotSchema,
} from '@ikenga/contract/ngwa';

// ── Strict parity validator (DEC-26) ─────────────────────────────────────────
// In standard Zod usage, schemas strip unknown keys by default.
// To prevent silent producer/consumer drift between the Rust producer (ngwa.rs)
// and the TypeScript contract (@ikenga/contract/ngwa), DEC-26 requires a test-only
// strict validator that asserts zero unknown keys at any nesting depth.

export function assertNoUnknownKeys(raw: unknown, parsed: unknown, path = '$'): void {
	if (raw === null || raw === undefined) return;
	if (typeof raw !== 'object') return;
	if (Array.isArray(raw)) {
		if (!Array.isArray(parsed)) {
			throw new Error(`Type mismatch at ${path}: expected array`);
		}
		for (let i = 0; i < raw.length; i++) {
			assertNoUnknownKeys(raw[i], parsed[i], `${path}[${i}]`);
		}
		return;
	}
	if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
		throw new Error(`Type mismatch at ${path}: expected object`);
	}
	const parsedObj = parsed as Record<string, unknown>;
	const rawObj = raw as Record<string, unknown>;
	for (const key of Object.keys(rawObj)) {
		if (!(key in parsedObj)) {
			throw new Error(`Unrecognized key "${key}" at ${path}.${key} (strict parity violation)`);
		}
		assertNoUnknownKeys(rawObj[key], parsedObj[key], `${path}.${key}`);
	}
}

export type StrictValidationResult =
	| { success: true; data: NgwaSnapshot }
	| { success: false; error: Error };

export function parseStrictNgwaSnapshot(data: unknown): NgwaSnapshot {
	// 1. Zod schema validation (types, required fields, enums, nullabilities)
	const parsed = NgwaSnapshotSchema.parse(data);

	// 2. Strict parity check (no unknown keys at any level)
	assertNoUnknownKeys(data, parsed);

	// 3. Domain invariant: unmeasurable kinds must have null usage
	// 4. Domain invariant: ID uniqueness
	const seenIds = new Set<string>();
	for (let i = 0; i < parsed.items.length; i++) {
		const item = parsed.items[i];
		if (seenIds.has(item.id)) {
			throw new Error(`Duplicate item id: ${item.id} at index ${i}`);
		}
		seenIds.add(item.id);

		if ((item.kind === 'hook' || item.kind === 'command') && item.usage !== null) {
			throw new Error(
				`Unmeasurable kind "${item.kind}" must have null usage, got non-null at index ${i}`
			);
		}
	}

	return parsed;
}

export function safeParseStrictNgwaSnapshot(data: unknown): StrictValidationResult {
	try {
		const dataParsed = parseStrictNgwaSnapshot(data);
		return { success: true, data: dataParsed };
	} catch (err) {
		return { success: false, error: err as Error };
	}
}

// ── Test Fixtures ─────────────────────────────────────────────────────────────

function createValidSnapshot(): NgwaSnapshot {
	const appItem: NgwaItem = {
		id: 'com.ikenga.studio',
		kind: 'app',
		name: 'studio',
		display_name: 'Studio',
		description: 'Beat-detection workbench.',
		version: '0.4.2',
		latest_version: '0.5.0',
		scope: { kind: 'personal' },
		origin: {
			source: 'registry',
			url: 'https://registry.ikenga.dev/index.json',
			ref: null,
			resolved_version: '0.4.2',
			publisher: 'ikenga-hq',
			managed: true,
			auto_update: false,
			installed_at_ms: 1_726_000_000_000,
			updated_at_ms: 1_726_900_000_000,
		},
		state: 'enabled',
		runtime: {
			state: 'running',
			pid: 44_112,
			uptime_s: 903,
			restarts: 1,
			last_err: null,
			last_crash_ms: null,
		},
		trust: {
			state: 'granted',
			signed: true,
			auto_trusted: false,
			review_pending: false,
			perms: {
				shell_execute: ['git *'],
				fs_write_outside_sandbox: ['~/.claude/**'],
				net: ['https://api.royalti.io/**'],
				vault_keys: ['supabase.anon'],
			},
			last_granted_at_ms: 1_726_900_000_000,
		},
		placements: [
			{
				engine: 'claude',
				scope: { kind: 'project', project_id: 'C--Users-x-royalti-co' },
				path: '/Users/x/royalti-co/.claude/skills/groundwork',
				mechanism: 'symlink-dir',
				present: true,
				link_target: '/Users/x/.ikenga/oba/skills/groundwork',
				in_store: true,
				managed_by: 'oba',
				overridden_by: null,
				format: 'md-yaml',
				status: 'active',
			},
		],
		usage: {
			source: 'transcript',
			last_used_ms: 1_726_950_000_000,
			count_7d: 12,
			count_30d: 72,
			tokens_30d: 1_240_991,
			window_start_ms: 1_724_000_000_000,
		},
		requires: [
			{
				kind: 'skill',
				name: 'skill-core',
				item_id: 'skill:personal:skill-core',
				source: 'catalog',
				ref: null,
			},
		],
		required_by: [],
		owner_pkg_id: null,
		install_path: '/Users/x/.ikenga/pkgs/com.ikenga.studio',
		engines: ['claude'],
	};

	const hookItem: NgwaItem = {
		id: 'hook:personal:pre-tool-enforce',
		kind: 'hook',
		name: 'pre-tool-enforce',
		display_name: 'pre-tool-enforce',
		description: 'Enforce tool execution boundaries.',
		version: null,
		latest_version: null,
		scope: { kind: 'personal' },
		origin: {
			source: 'local',
			url: null,
			ref: null,
			resolved_version: null,
			publisher: null,
			managed: false,
			auto_update: false,
			installed_at_ms: null,
			updated_at_ms: null,
		},
		state: 'enabled',
		runtime: null,
		trust: {
			state: 'not_applicable',
			signed: false,
			auto_trusted: false,
			review_pending: false,
			perms: null,
			last_granted_at_ms: null,
		},
		placements: [
			{
				engine: 'claude',
				scope: { kind: 'personal' },
				path: '/Users/x/.claude/hooks/pre-tool.json',
				mechanism: 'file',
				present: true,
				link_target: null,
				in_store: false,
				managed_by: 'user',
				overridden_by: null,
				format: 'json-embedded',
				status: 'active',
			},
		],
		usage: null, // Hook must have null usage
		requires: [],
		required_by: [],
		owner_pkg_id: null,
		install_path: null,
		engines: ['claude'],
	};

	const measuredZeroItem: NgwaItem = {
		id: 'skill:personal:unused-helper',
		kind: 'skill',
		name: 'unused-helper',
		display_name: 'unused-helper',
		description: 'Helper skill with 0 measured usage.',
		version: '1.0.0',
		latest_version: null,
		scope: { kind: 'personal' },
		origin: {
			source: 'local',
			url: null,
			ref: null,
			resolved_version: null,
			publisher: null,
			managed: false,
			auto_update: false,
			installed_at_ms: null,
			updated_at_ms: null,
		},
		state: 'enabled',
		runtime: null,
		trust: {
			state: 'not_applicable',
			signed: false,
			auto_trusted: false,
			review_pending: false,
			perms: null,
			last_granted_at_ms: null,
		},
		placements: [],
		usage: {
			source: 'transcript',
			last_used_ms: null,
			count_7d: 0,
			count_30d: 0,
			tokens_30d: 0,
			window_start_ms: 1_724_000_000_000,
		},
		requires: [],
		required_by: [],
		owner_pkg_id: null,
		install_path: null,
		engines: ['claude'],
	};

	const sourceHealth = { ok: true, error: null, count: 1 };

	return {
		items: [appItem, hookItem, measuredZeroItem],
		as_of_ms: 1_726_951_000_000,
		sources: {
			kernel: sourceHealth,
			oba: sourceHealth,
			engine_config: sourceHealth,
			engine_assets: { ok: false, error: 'no engine assets', count: 0 },
			trust: sourceHealth,
			usage: sourceHealth,
		},
	};
}

/** Recursively asserts all keys in an object tree are snake_case. */
function assertSnakeCaseKeys(value: unknown, path = '$'): void {
	if (Array.isArray(value)) {
		value.forEach((v, i) => assertSnakeCaseKeys(v, `${path}[${i}]`));
		return;
	}
	if (value === null || typeof value !== 'object') return;
	for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
		expect(k, `key ${path}.${k} is not snake_case`).toMatch(/^[a-z][a-z0-9]*(_[a-z0-9]+)*$/);
		assertSnakeCaseKeys(v, `${path}.${k}`);
	}
}

// ── Test Suites ───────────────────────────────────────────────────────────────

describe('WP-14: DEC-26 Strict Parity Validator', () => {
	it('accepts a fully compliant NgwaSnapshot fixture against strict validator', () => {
		const fixture = createValidSnapshot();
		const parsed = parseStrictNgwaSnapshot(fixture);
		expect(parsed.items.length).toBe(3);
		expect(parsed.sources.kernel.ok).toBe(true);
		assertSnakeCaseKeys(fixture);
	});

	it('preserves usage nullability distinction: null = unmeasured, zeros = measured 0', () => {
		const fixture = createValidSnapshot();
		const hook = fixture.items.find((i) => i.id === 'hook:personal:pre-tool-enforce');
		const unused = fixture.items.find((i) => i.id === 'skill:personal:unused-helper');

		expect(hook?.usage).toBeNull();
		expect(unused?.usage).not.toBeNull();
		expect(unused?.usage?.count_7d).toBe(0);
		expect(unused?.usage?.count_30d).toBe(0);
		expect(unused?.usage?.tokens_30d).toBe(0);
		expect(unused?.usage?.last_used_ms).toBeNull();
	});

	describe('Mutation tests: zero drift enforcement', () => {
		it('fails when an unknown field is injected at snapshot root', () => {
			const mutated = {
				...createValidSnapshot(),
				unexpected_field: 'leak',
			};
			const res = safeParseStrictNgwaSnapshot(mutated);
			expect(res.success).toBe(false);
			if (!res.success) {
				expect(res.error.message).toContain('unexpected_field');
				expect(res.error.message).toContain('strict parity violation');
			}
		});

		it('fails when an unknown field is injected into an item', () => {
			const fixture = createValidSnapshot();
			(fixture.items[0] as Record<string, unknown>).rogue_property = 'drift';
			const res = safeParseStrictNgwaSnapshot(fixture);
			expect(res.success).toBe(false);
			if (!res.success) {
				expect(res.error.message).toContain('rogue_property');
				expect(res.error.message).toContain('strict parity violation');
			}
		});

		it('fails when an unknown field is injected into origin', () => {
			const fixture = createValidSnapshot();
			(fixture.items[0].origin as Record<string, unknown>).git_commit = 'abc1234';
			const res = safeParseStrictNgwaSnapshot(fixture);
			expect(res.success).toBe(false);
			if (!res.success) {
				expect(res.error.message).toContain('git_commit');
				expect(res.error.message).toContain('strict parity violation');
			}
		});

		it('fails when an unknown field is injected into trust', () => {
			const fixture = createValidSnapshot();
			(fixture.items[0].trust as Record<string, unknown>).fingerprint = 'sha256:123';
			const res = safeParseStrictNgwaSnapshot(fixture);
			expect(res.success).toBe(false);
			if (!res.success) {
				expect(res.error.message).toContain('fingerprint');
				expect(res.error.message).toContain('strict parity violation');
			}
		});

		it('fails when an unknown field is injected into placement', () => {
			const fixture = createValidSnapshot();
			(fixture.items[0].placements[0] as Record<string, unknown>).extra_prop = 'val';
			const res = safeParseStrictNgwaSnapshot(fixture);
			expect(res.success).toBe(false);
			if (!res.success) {
				expect(res.error.message).toContain('extra_prop');
				expect(res.error.message).toContain('strict parity violation');
			}
		});

		it('fails when a required field is missing from an item', () => {
			const fixture = createValidSnapshot();
			const itemWithoutScope = { ...fixture.items[0] };
			delete (itemWithoutScope as Record<string, unknown>).scope;
			fixture.items[0] = itemWithoutScope as NgwaItem;

			const res = safeParseStrictNgwaSnapshot(fixture);
			expect(res.success).toBe(false);
		});

		it('fails when a required field is missing from sources health', () => {
			const fixture = createValidSnapshot();
			const sourcesWithoutKernel = { ...fixture.sources };
			delete (sourcesWithoutKernel as Record<string, unknown>).kernel;
			(fixture as Record<string, unknown>).sources = sourcesWithoutKernel;

			const res = safeParseStrictNgwaSnapshot(fixture);
			expect(res.success).toBe(false);
		});

		it('fails when non-null usage is assigned to a hook item', () => {
			const fixture = createValidSnapshot();
			const hook = fixture.items.find((i) => i.id === 'hook:personal:pre-tool-enforce');
			expect(hook).toBeDefined();
			if (hook) {
				hook.usage = {
					source: 'transcript',
					last_used_ms: Date.now(),
					count_7d: 1,
					count_30d: 5,
					tokens_30d: 100,
					window_start_ms: 1000,
				};
			}

			const res = safeParseStrictNgwaSnapshot(fixture);
			expect(res.success).toBe(false);
			if (!res.success) {
				expect(res.error.message).toContain('Unmeasurable kind "hook"');
			}
		});

		it('fails when duplicate item IDs exist in snapshot', () => {
			const fixture = createValidSnapshot();
			fixture.items.push({
				...fixture.items[0], // Duplicate ID
			});

			const res = safeParseStrictNgwaSnapshot(fixture);
			expect(res.success).toBe(false);
			if (!res.success) {
				expect(res.error.message).toContain('Duplicate item id');
			}
		});
	});
});
