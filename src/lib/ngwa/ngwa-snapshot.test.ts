// DEC-26 parity guard for `ngwa_snapshot` (WP-14 / WP-14a).
//
// The payload under test is NOT hand-written here. It is the committed golden
// `__fixtures__/ngwa-snapshot.golden.json`, which the Rust test
// `commands::ngwa::tests::golden_snapshot_matches_committed_file` produces from
// the real `build_snapshot` and compares byte-for-byte. So a field added,
// renamed or re-cased on the Rust side fails that Rust test first, and — once
// the golden is regenerated — fails the recursively strict parse below.
//
// The shipped `NgwaSnapshotSchema` stays permissive (DEC-26: freezing means
// freezing); strictness lives only in this test, via `deepStrict`.

import { describe, expect, it } from 'vitest';
import { NGWA_KINDS, type NgwaSnapshot, NgwaSnapshotSchema } from '@ikenga/contract/ngwa';
import golden from './__fixtures__/ngwa-snapshot.golden.json';

// ── Recursively strict variant of a (zod 3) contract schema ──────────────────
//
// The contract package builds its schemas with its own zod 3, so the schema
// objects are rebuilt through their own constructors rather than through the
// shell's zod 4. Any schema kind not listed throws, so a new construct in the
// contract cannot silently escape strictness.

interface ZodLike {
	_def: { typeName: string } & Record<string, unknown>;
	[key: string]: unknown;
}
type Ctor = (new (def: Record<string, unknown>) => ZodLike) & {
	create: (...args: unknown[]) => ZodLike;
};

function deepStrict(schema: ZodLike): ZodLike {
	const def = schema._def;
	const ctor = schema.constructor as Ctor;
	switch (def.typeName) {
		case 'ZodObject': {
			const shape = (schema.shape as Record<string, ZodLike>) ?? {};
			const strictShape = Object.fromEntries(
				Object.entries(shape).map(([k, v]) => [k, deepStrict(v)])
			);
			const extended = (schema.extend as (s: Record<string, ZodLike>) => ZodLike)(strictShape);
			return (extended.strict as () => ZodLike)();
		}
		case 'ZodArray':
			return new ctor({ ...def, type: deepStrict(def.type as ZodLike) });
		case 'ZodNullable':
		case 'ZodOptional':
			return new ctor({ ...def, innerType: deepStrict(def.innerType as ZodLike) });
		case 'ZodUnion':
			return new ctor({ ...def, options: (def.options as ZodLike[]).map(deepStrict) });
		case 'ZodDiscriminatedUnion':
			return ctor.create(def.discriminator, (def.options as ZodLike[]).map(deepStrict));
		case 'ZodString':
		case 'ZodNumber':
		case 'ZodBoolean':
		case 'ZodEnum':
		case 'ZodLiteral':
			return schema;
		default:
			throw new Error(`deepStrict: unhandled schema type ${def.typeName}`);
	}
}

const StrictSnapshot = deepStrict(NgwaSnapshotSchema as unknown as ZodLike) as unknown as {
	safeParse: (v: unknown) => { success: boolean; data?: NgwaSnapshot; error?: Error };
};

/** Strict schema parse + the snapshot-level invariants gate §2 states. */
function checkSnapshot(raw: unknown): { ok: true; data: NgwaSnapshot } | { ok: false; error: string } {
	const res = StrictSnapshot.safeParse(raw);
	if (!res.success || !res.data) return { ok: false, error: String(res.error) };
	const seen = new Set<string>();
	for (const item of res.data.items) {
		if (seen.has(item.id)) return { ok: false, error: `Duplicate item id: ${item.id}` };
		seen.add(item.id);
		if (['hook', 'command', 'schedule'].includes(item.kind) && item.usage !== null) {
			return { ok: false, error: `Unmeasurable kind "${item.kind}" has non-null usage (${item.id})` };
		}
		if (item.latest_version !== null) {
			return { ok: false, error: `latest_version must be null from the snapshot (${item.id})` };
		}
	}
	return { ok: true, data: res.data };
}

const clone = (): Record<string, any> => structuredClone(golden) as Record<string, any>;

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

// ── Tests ────────────────────────────────────────────────────────────────────

describe('DEC-26: the Rust producer golden parses under a recursively strict contract', () => {
	it('parses with zero unknown keys and nothing stripped', () => {
		const res = checkSnapshot(golden);
		if (!res.ok) throw new Error(res.error);
		// If strictness were not applied, stripped keys would make these differ.
		expect(res.data).toEqual(golden);
	});

	it('uses snake_case keys throughout', () => {
		assertSnakeCaseKeys(golden);
	});

	it('exercises every kind WP-14 emits, both usage states, an override and a dependency edge', () => {
		const items = (golden as unknown as NgwaSnapshot).items;
		const kinds = new Set(items.map((i) => i.kind));
		const emitted = NGWA_KINDS.filter((k) => !['bundle', 'workflow'].includes(k));
		for (const k of emitted) expect(kinds, `golden lacks a "${k}" item`).toContain(k);
		expect(items.some((i) => i.usage === null)).toBe(true);
		expect(items.some((i) => i.usage !== null && i.usage.count_30d === 0)).toBe(true);
		expect(items.some((i) => i.usage !== null && (i.usage.count_30d ?? 0) > 0)).toBe(true);
		expect(items.some((i) => i.placements.some((p) => p.overridden_by !== null))).toBe(true);
		expect(items.some((i) => i.required_by.length > 0)).toBe(true);
	});

	it('deepStrict really is strict at every nesting level it rebuilt', () => {
		// Guard against the helper silently degrading to the permissive schema.
		const probe = clone();
		probe.items[0].placements[0].scope.extra = 1;
		expect(checkSnapshot(probe).ok).toBe(false);
		expect(NgwaSnapshotSchema.safeParse(probe).success).toBe(true);
	});
});

describe('DEC-26 mutation tests: drift in the producer payload fails', () => {
	const unknownKeyAt: Array<[string, (s: Record<string, any>) => void]> = [
		['snapshot root', (s) => (s.unexpected_field = 1)],
		['sources entry', (s) => (s.sources.kernel.extra = 1)],
		['item', (s) => (s.items[0].rogue_property = 'x')],
		['item.scope', (s) => (s.items.find((i: any) => i.scope.kind === 'project').scope.extra = 1)],
		['item.origin', (s) => (s.items[0].origin.git_commit = 'abc')],
		['item.trust', (s) => (s.items[0].trust.fingerprint = 'x')],
		['item.trust.perms', (s) => (s.items.find((i: any) => i.trust.perms).trust.perms.extra = [])],
		['item.runtime', (s) => (s.items.find((i: any) => i.runtime).runtime.extra = 1)],
		['item.placements[]', (s) => (s.items.find((i: any) => i.placements.length).placements[0].extra = 1)],
		['item.usage', (s) => (s.items.find((i: any) => i.usage).usage.extra = 1)],
		['item.requires[]', (s) => (s.items.find((i: any) => i.requires.length).requires[0].extra = 1)],
		['item.required_by[]', (s) => (s.items.find((i: any) => i.required_by.length).required_by[0].extra = 1)],
	];
	for (const [where, mutate] of unknownKeyAt) {
		it(`rejects an unknown key in ${where}`, () => {
			const s = clone();
			mutate(s);
			const res = checkSnapshot(s);
			expect(res.ok).toBe(false);
			if (!res.ok) expect(res.error).toMatch(/[Uu]nrecognized key/);
		});
	}

	it('rejects a camelCase rename of a field (display_name → displayName)', () => {
		const s = clone();
		s.items[0].displayName = s.items[0].display_name;
		delete s.items[0].display_name;
		expect(checkSnapshot(s).ok).toBe(false);
	});

	it('rejects a missing required field', () => {
		const s = clone();
		delete s.items[0].scope;
		expect(checkSnapshot(s).ok).toBe(false);
		const t = clone();
		delete t.sources.kernel;
		expect(checkSnapshot(t).ok).toBe(false);
	});

	it('rejects an out-of-union enum value', () => {
		const s = clone();
		s.items.find((i: any) => i.runtime).runtime.state = 'zombie';
		expect(checkSnapshot(s).ok).toBe(false);
	});

	it('rejects non-null usage on a hook', () => {
		const s = clone();
		const hook = s.items.find((i: any) => i.kind === 'hook');
		hook.usage = {
			source: 'transcript',
			last_used_ms: 1,
			count_7d: 1,
			count_30d: 1,
			tokens_30d: 1,
			window_start_ms: 0,
		};
		const res = checkSnapshot(s);
		expect(res.ok).toBe(false);
		if (!res.ok) expect(res.error).toContain('Unmeasurable kind "hook"');
	});

	it('rejects duplicate item ids', () => {
		const s = clone();
		s.items.push(structuredClone(s.items[0]));
		const res = checkSnapshot(s);
		expect(res.ok).toBe(false);
		if (!res.ok) expect(res.error).toContain('Duplicate item id');
	});
});
