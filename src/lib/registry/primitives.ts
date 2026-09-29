// Primitive catalog (Ọba WP-10) — the "recommended / available to install"
// feed for standalone Claude-config primitives (skills/agents/commands/hooks/
// mcp). The primitive-level analog of the pkg registry index (./client.ts).
//
// SOURCE (decided 2026-05-28, see plans/oba-registry/04-discussion.md Round 3):
// a dedicated, signed `primitives.json` published to the ikenga-registry
// (GitHub Pages), fetched + minisign-verified exactly like the pkg index.
// WP-10b (this change): `fetchPrimitiveCatalog` now fetches `primitives.json`
// + its `.minisig` from `PRIMITIVES_URL`, verifies the signature against the
// same `REGISTRY_PUBKEY` the pkg index trusts, and validates the payload shape.
// The BUNDLED `primitives-seed.json` is kept as a fallback used ONLY when the
// remote is genuinely ABSENT (network failure / 404) — never when the signature
// or shape fails to verify (a verify failure is treated as hostile and throws,
// mirroring `client.ts::fetchIndex`).
//
// Install / update actions are gated on Phase 2 (git/npx install + update,
// WP-07–09) — a catalog entry is only a POINTER that resolves to a git/npx
// master, so it cannot be installed until that machinery exists. Until then
// the UI shows Install/Update disabled-pending.

import { useQuery } from '@tanstack/react-query';
import { semverCompare, verifyMinisign } from '@ikenga/registry-client';

import type {
	CatalogEntryRef,
	ClaudeStoreEntry,
	ClaudeStoreKind,
	ObaCatalogPin,
	ObaPin,
	RequiresEntry,
} from '@/lib/tauri-cmd';
import { PRIMITIVES_URL, REGISTRY_PUBKEY } from './client';
import seed from './primitives-seed.json';

/** One installable primitive in the Ọba catalog. `source`/`url` are the
 *  discovery origin the Phase-2 installer resolves (`source:"catalog"` is
 *  recorded as provenance once installed). */
export interface PrimitiveCatalogEntry {
	kind: ClaudeStoreKind;
	name: string;
	version: string;
	description: string | null;
	/** Where install resolves under the hood (Phase 2). */
	source: 'git' | 'npx';
	/** git remote URL | npm/skills spec. */
	url: string;
	publisher?: string | null;
	/** Forward dependencies (ADR-015 §3 / WP-15) — the compiled `requires` list
	 *  the publish-time lift writes into the published manifest and which the
	 *  catalog generator auto-carries (the manifest is embedded in each entry).
	 *  Carried on the catalog so the install consent surface can list "also
	 *  installs: <dep> from <provenance>" BEFORE any fetch. The Rust resolver
	 *  remains authoritative at install (it re-reads each fetched manifest);
	 *  this field only drives the pre-install consent display. Absent → no deps. */
	requires?: RequiresEntry[];
	/** Member skills a `bundle` catalog row carries (WP-18). Derived at publish
	 *  into the catalog (later WP); the bundle installer (WP-19) places these.
	 *  Optional/default-empty so a non-bundle / pre-WP-18 catalog row (no
	 *  `members`) still parses. Mirrors `members` on the Rust `CatalogEntryRef`. */
	members?: string[];
	/** R57 · Q3: the pin the signature covers — a git commit SHA (7–40 hex), or
	 *  a tag / version when it is not hex. Absent → unpinned: the entry vouches
	 *  for *where* only, and installs whatever the source serves today. */
	ref?: string;
	/** R57 · Q3: content hash of the primitive (`sha256-<64 hex>`), verified by
	 *  the installer against what it fetched. */
	hash?: string;
}

export interface PrimitiveCatalog {
	$schemaVersion: number;
	updatedAt: string;
	primitives: PrimitiveCatalogEntry[];
}

const KINDS: ReadonlySet<string> = new Set([
	'skill',
	'agent',
	'command',
	'hook',
	'mcp',
	// WP-18: a `bundle` is a first-class kind (a package shipping N member
	// skills). Accepted on catalog entries and in `requires[]` so a bundle row /
	// dependency is not rejected by `parseCatalog`/`parseRequires`.
	'bundle',
]);
const REQUIRE_SOURCES: ReadonlySet<string> = new Set(['git', 'npx', 'catalog', 'local']);

/** Validate a catalog entry's optional `requires` array (WP-15). Mirrors the
 *  `RequiresEntry` shape ({kind, name, source?, ref?}); throws on a malformed
 *  element so a verified-but-junk catalog is rejected loudly. Returns undefined
 *  when absent (= no deps). */
function parseRequires(raw: unknown, i: number): RequiresEntry[] | undefined {
	if (raw === undefined || raw === null) return undefined;
	if (!Array.isArray(raw)) {
		throw new Error(`primitives.json: entry ${i} \`requires\` is not an array`);
	}
	return raw.map((r, j) => {
		if (typeof r !== 'object' || r === null) {
			throw new Error(`primitives.json: entry ${i} requires[${j}] is not an object`);
		}
		const e = r as Record<string, unknown>;
		if (typeof e.kind !== 'string' || !KINDS.has(e.kind)) {
			throw new Error(
				`primitives.json: entry ${i} requires[${j}] has invalid kind ${String(e.kind)}`
			);
		}
		if (typeof e.name !== 'string') {
			throw new Error(`primitives.json: entry ${i} requires[${j}] missing name`);
		}
		if (
			e.source !== undefined &&
			(typeof e.source !== 'string' || !REQUIRE_SOURCES.has(e.source))
		) {
			throw new Error(
				`primitives.json: entry ${i} requires[${j}] has invalid source ${String(e.source)}`
			);
		}
		if (e.ref !== undefined && typeof e.ref !== 'string') {
			throw new Error(`primitives.json: entry ${i} requires[${j}] has non-string ref`);
		}
		return {
			kind: e.kind,
			name: e.name,
			...(e.source !== undefined ? { source: e.source as RequiresEntry['source'] } : {}),
			...(e.ref !== undefined ? { ref: e.ref as string } : {}),
		};
	});
}

/** A git commit SHA (abbreviated to at least 7, at most 40 hex chars). */
const SHA_RE = /^[0-9a-f]{7,40}$/i;
/** A catalog content hash: `sha256-` + 64 hex chars. */
const HASH_RE = /^sha256-[0-9a-f]{64}$/i;

/** Validate an entry's optional R57 pin (`ref` / `hash`). Throws on a malformed
 *  value — a signed catalog that carries a junk pin is rejected, never read as
 *  "unpinned". Absent fields stay absent (backward compatible). */
function parsePin(e: Record<string, unknown>, i: number): { ref?: string; hash?: string } {
	const out: { ref?: string; hash?: string } = {};
	if (e.ref !== undefined && e.ref !== null) {
		if (typeof e.ref !== 'string' || !e.ref.trim() || /\s/.test(e.ref)) {
			throw new Error(`primitives.json: entry ${i} has invalid ref ${JSON.stringify(e.ref)}`);
		}
		out.ref = e.ref;
	}
	if (e.hash !== undefined && e.hash !== null) {
		if (typeof e.hash !== 'string' || !HASH_RE.test(e.hash)) {
			throw new Error(
				`primitives.json: entry ${i} has invalid hash ${JSON.stringify(e.hash)} (want sha256-<64 hex>)`
			);
		}
		out.hash = e.hash;
	}
	return out;
}

/** Defensive runtime validation of a fetched catalog payload (there is no Zod
 *  schema for `primitives.json` in @ikenga/contract — the pkg index has one,
 *  primitives don't yet). Throws on a malformed payload so a verified-but-junk
 *  catalog is rejected loudly rather than rendering garbage rows. */
export function parseCatalog(json: unknown): PrimitiveCatalogEntry[] {
	if (typeof json !== 'object' || json === null) {
		throw new Error('primitives.json: not an object');
	}
	const obj = json as Record<string, unknown>;
	if (!Array.isArray(obj.primitives)) {
		throw new Error('primitives.json: missing `primitives` array');
	}
	return obj.primitives.map((raw, i) => {
		if (typeof raw !== 'object' || raw === null) {
			throw new Error(`primitives.json: entry ${i} is not an object`);
		}
		const e = raw as Record<string, unknown>;
		if (typeof e.kind !== 'string' || !KINDS.has(e.kind)) {
			throw new Error(`primitives.json: entry ${i} has invalid kind ${String(e.kind)}`);
		}
		if (typeof e.name !== 'string' || typeof e.version !== 'string') {
			throw new Error(`primitives.json: entry ${i} missing name/version`);
		}
		if (e.source !== 'git' && e.source !== 'npx') {
			throw new Error(`primitives.json: entry ${i} has invalid source ${String(e.source)}`);
		}
		if (typeof e.url !== 'string') {
			throw new Error(`primitives.json: entry ${i} missing url`);
		}
		return {
			kind: e.kind as ClaudeStoreKind,
			name: e.name,
			version: e.version,
			description: typeof e.description === 'string' ? e.description : null,
			source: e.source,
			url: e.url,
			publisher: typeof e.publisher === 'string' ? e.publisher : null,
			requires: parseRequires(e.requires, i),
			...(Array.isArray(e.members)
				? { members: e.members.filter((m): m is string => typeof m === 'string') }
				: {}),
			...parsePin(e, i),
		};
	});
}

/** The catalog plus where it came from: `verified` = the remote, minisign-
 *  verified; false = the bundled seed (the remote was absent). */
export interface PrimitiveCatalogResult {
	entries: PrimitiveCatalogEntry[];
	verified: boolean;
}

/** Fetch + minisign-verify the remote primitive catalog (mirrors
 *  `client.ts::fetchIndex`). Returns the bundled seed only when the remote is
 *  genuinely ABSENT (network failure / non-2xx) — a signature or shape failure
 *  THROWS, so a tampered catalog is never silently substituted with the seed. */
export async function fetchPrimitiveCatalog(
	signal?: AbortSignal
): Promise<PrimitiveCatalogEntry[]> {
	return (await fetchPrimitiveCatalogResult(signal)).entries;
}

/** {@link fetchPrimitiveCatalog}, telling the verified remote from the seed. */
export async function fetchPrimitiveCatalogResult(
	signal?: AbortSignal
): Promise<PrimitiveCatalogResult> {
	const seedResult = (): PrimitiveCatalogResult => ({
		entries: (seed as PrimitiveCatalog).primitives,
		verified: false,
	});
	const sigUrl = `${PRIMITIVES_URL}.minisig`;
	let raw: Uint8Array;
	let signature: string;
	try {
		const [catRes, sigRes] = await Promise.all([
			fetch(PRIMITIVES_URL, { signal }),
			fetch(sigUrl, { signal }),
		]);
		if (!catRes.ok || !sigRes.ok) {
			// Remote absent (e.g. catalog not yet published / 404). Degrade to the
			// bundled seed rather than failing the surface.
			console.warn(
				`[primitives] remote catalog unavailable (${catRes.status}/${sigRes.status}); using bundled seed`
			);
			return seedResult();
		}
		raw = new Uint8Array(await catRes.arrayBuffer());
		signature = await sigRes.text();
	} catch (err) {
		// Network error — treat as absent, fall back to the seed.
		console.warn(
			`[primitives] remote catalog fetch failed (${(err as Error).message}); using bundled seed`
		);
		return seedResult();
	}

	// Verify BEFORE parsing — same trust ordering as fetchIndex. A failure here
	// is hostile, not "absent", so it throws and is NOT replaced by the seed.
	const ok = await verifyMinisign(raw, signature, REGISTRY_PUBKEY);
	if (!ok) {
		throw new Error(
			'primitives.json signature did not verify against the configured registry public key'
		);
	}

	let json: unknown;
	try {
		json = JSON.parse(new TextDecoder().decode(raw));
	} catch (err) {
		throw new Error(`primitives.json is not valid JSON: ${(err as Error).message}`);
	}
	return { entries: parseCatalog(json), verified: true };
}

export const primitiveCatalogKey = ['registry', 'primitives'] as const;

const catalogQueryOptions = {
	// The cached value is the {entries, verified} result; `usePrimitiveCatalog`
	// selects the entries, `usePrimitiveCatalogResult` keeps the flag.
	queryKey: [...primitiveCatalogKey, 'result'] as const,
	queryFn: ({ signal }: { signal?: AbortSignal }) => fetchPrimitiveCatalogResult(signal),
	// WP-10b fetches the remote signed catalog over the network, so cache it
	// to match the pkg index cadence rather than re-fetching on every mount.
	staleTime: 6 * 60 * 60 * 1000, // ~6h
	refetchOnWindowFocus: false,
};

const selectEntries = (r: PrimitiveCatalogResult) => r.entries;

export function usePrimitiveCatalog(opts: { enabled?: boolean } = {}) {
	return useQuery({
		...catalogQueryOptions,
		enabled: opts.enabled ?? true,
		select: selectEntries,
	});
}

/** The catalog with its `verified` flag (the Store's "catalog signed" line). */
export function usePrimitiveCatalogResult(opts: { enabled?: boolean } = {}) {
	return useQuery({ ...catalogQueryOptions, enabled: opts.enabled ?? true });
}

// ─── R57 · Q3 — pins ─────────────────────────────────────────────────────────

/** True when `ref` reads as a git commit SHA (7–40 hex). */
export function isShaRef(ref: string | null | undefined): ref is string {
	return typeof ref === 'string' && SHA_RE.test(ref);
}

/** The pin an install/update of `entry` must match, or null when the entry is
 *  unpinned. `sha` only when `ref` is a SHA (a tag/version `ref` is a git ref
 *  to fetch, not a pin); `hash` whenever the entry carries one. */
export function catalogPin(entry: PrimitiveCatalogEntry): ObaPin | null {
	const sha = isShaRef(entry.ref) ? entry.ref : null;
	const hash = entry.hash ?? null;
	if (!sha && !hash) return null;
	return { sha, hash };
}

/** True when the catalog pins `entry` (a SHA and/or a content hash). */
export function isPinned(entry: PrimitiveCatalogEntry): boolean {
	return catalogPin(entry) !== null;
}

/** The git ref to fetch `entry` at when it is not SHA-pinned (a tag/branch
 *  `ref`), else null (the default branch — or the pinned SHA, via the pin). */
export function catalogGitRef(entry: PrimitiveCatalogEntry): string | null {
	return entry.ref && !isShaRef(entry.ref) ? entry.ref : null;
}

/** Two SHAs name the same commit: case-insensitive, one a prefix of the other
 *  (the catalog may pin an abbreviated SHA; the vault records the full one). */
export function shaMatches(a: string | null | undefined, b: string | null | undefined): boolean {
	if (!a || !b) return false;
	const x = a.toLowerCase();
	const y = b.toLowerCase();
	return x.startsWith(y) || y.startsWith(x);
}

/** A SHA shortened for display (7 chars); other strings are returned as-is. */
export function shortSha(sha: string | null | undefined): string {
	if (!sha) return '—';
	return SHA_RE.test(sha) ? sha.slice(0, 7) : sha;
}

/**
 * True when an installed catalog entry is behind its catalog pin: the
 * recorded `version` is not the pinned SHA (prefix compare either way), or the
 * recorded content `hash` differs from the pinned hash. A store entry with no
 * recorded hash (pre-R57) is judged by the SHA when the pin has one, and
 * counts as behind only when the pin is hash-only (nothing else to compare).
 * An unpinned entry is never "behind" by this test.
 */
export function catalogPinMoved(store: ClaudeStoreEntry, entry: PrimitiveCatalogEntry): boolean {
	const pin = catalogPin(entry);
	if (!pin) return false;
	if (pin.sha && !shaMatches(store.version ?? null, pin.sha)) return true;
	if (pin.hash) {
		if (store.hash) return store.hash.toLowerCase() !== pin.hash.toLowerCase();
		return !pin.sha;
	}
	return false;
}

/** The catalog snapshot `obaInstallWithDeps` resolves deps against, pins
 *  included so every catalogued dep is fetched at its pin. */
export function catalogRefs(catalog: readonly PrimitiveCatalogEntry[]): CatalogEntryRef[] {
	return catalog.map((c) => ({
		kind: c.kind,
		name: c.name,
		source: c.source,
		url: c.url,
		...(c.members ? { members: c.members } : {}),
		...(c.ref ? { ref: c.ref } : {}),
		...(c.hash ? { hash: c.hash } : {}),
	}));
}

/** The pins `obaAutoUpdateAll` moves pinned catalog installs to (Q3). */
export function catalogPins(catalog: readonly PrimitiveCatalogEntry[]): ObaCatalogPin[] {
	const out: ObaCatalogPin[] = [];
	for (const c of catalog) {
		const pin = catalogPin(c);
		if (pin) out.push({ kind: c.kind, name: c.name, sha: pin.sha ?? null, hash: pin.hash ?? null });
	}
	return out;
}

export type PrimitiveStatus = 'installed' | 'updatable' | 'available';

/** One row in the merged store-surface view: a local store entry, a catalog
 *  recommendation, or both (installed + catalog-known). */
export interface PrimitiveViewItem {
	key: string;
	kind: ClaudeStoreKind;
	name: string;
	description: string | null;
	status: PrimitiveStatus;
	/** Present when installed (canonical copy in the local store). */
	store: ClaudeStoreEntry | null;
	/** Present when the catalog lists it. */
	catalog: PrimitiveCatalogEntry | null;
}

/** Merge the local store (what's installed) with the catalog (what's
 *  available) into one status-tagged list. Catalog-only entries are
 *  `available`. An installed entry is `updatable` when:
 *   - the catalog entry is pinned (R57 · Q3), the store entry was installed
 *     from the catalog (`fromCatalog`) and it has fallen behind the pin
 *     ({@link catalogPinMoved}) — a pinned catalog install moves only when the
 *     catalog moves the pin, and a same-named direct install is left alone;
 *   - the catalog entry is unpinned and its semver is newer than the recorded
 *     version (the pre-R57 rule; mostly moot, as the vault records SHAs).
 *  Store entries lead; catalog-only entries follow. */
export function mergePrimitiveView(
	store: ClaudeStoreEntry[],
	catalog: PrimitiveCatalogEntry[]
): PrimitiveViewItem[] {
	const catByKey = new Map<string, PrimitiveCatalogEntry>();
	for (const c of catalog) catByKey.set(`${c.kind}:${c.name}`, c);

	const out: PrimitiveViewItem[] = [];
	const installed = new Set<string>();
	for (const e of store) {
		const key = `${e.kind}:${e.name}`;
		installed.add(key);
		const cat = catByKey.get(key) ?? null;
		let updatable = false;
		if (cat && isPinned(cat)) updatable = e.fromCatalog === true && catalogPinMoved(e, cat);
		else if (cat && e.version != null) updatable = semverLess(e.version, cat.version);
		out.push({
			key: `store:${key}`,
			kind: e.kind,
			name: e.name,
			description: e.description,
			status: updatable ? 'updatable' : 'installed',
			store: e,
			catalog: cat,
		});
	}
	for (const c of catalog) {
		const key = `${c.kind}:${c.name}`;
		if (installed.has(key)) continue;
		out.push({
			key: `cat:${key}`,
			kind: c.kind,
			name: c.name,
			description: c.description,
			status: 'available',
			store: null,
			catalog: c,
		});
	}
	return out;
}

const SEMVER_RE = /^v?\d+\.\d+(\.\d+)?([-+].*)?$/;

/** `semverCompare(a, b) < 0`; false when either side is not semver — the
 *  vault records a git SHA as the version, which `semverCompare` would read
 *  as garbage numbers. */
function semverLess(a: string, b: string): boolean {
	if (!SEMVER_RE.test(a) || !SEMVER_RE.test(b)) return false;
	return semverCompare(a.replace(/^v/, ''), b.replace(/^v/, '')) < 0;
}

export const PRIMITIVE_STATUS_WORD: Record<PrimitiveStatus, string> = {
	installed: 'Installed',
	updatable: 'Update available',
	available: 'Available',
};

// ─── Forward-dependency consent surface (WP-15) ─────────────────────────────
// The catalog carries `requires`, so the consent dialog can list "X also
// installs: <dep> from <provenance>" with NO extra fetch. This mirrors the Rust
// resolver's closure shape (WP-13) but is a DISPLAY-ONLY pre-flight: the Rust
// `oba_install_with_deps` command re-reads each fetched manifest and remains the
// authoritative installer. We compute the closure here only to drive consent.

/** How a `requires` dependency resolves for the consent surface. */
export type ConsentResolution =
	/** Found in the signed catalog — provenance known, trust inherited from the
	 *  parent install's consent (no extra confirm). */
	| 'catalog'
	/** Not in the catalog but the `requires` entry self-pins a fetch source —
	 *  installable, but pulls un-catalogued code → needs an explicit extra confirm. */
	| 'pinned'
	/** Neither in the catalog nor self-pinned — the resolver cannot fetch it;
	 *  surfaced as a blocking warning + extra confirm. */
	| 'unresolved';

/** One row in the install consent surface — a single dependency in the closure
 *  of the primitive the user asked to install. */
export interface ConsentDep {
	kind: ClaudeStoreKind;
	name: string;
	resolution: ConsentResolution;
	/** Human-readable provenance for display (e.g. "npx · ikenga-hq/x"). */
	provenance: string;
	/** Already present in the local store — listed, not (re)installed. */
	satisfied: boolean;
	/** Requires the explicit extra confirm (non-catalog / un-pinned dep). */
	needsExtraConfirm: boolean;
}

/** Compute the forward-dependency closure of `target` from the catalog's
 *  `requires` edges, deduped + cycle-guarded, tagged with how each dep resolves
 *  and whether it's already satisfied. DISPLAY-ONLY (drives the WP-15 consent
 *  surface); the Rust resolver re-derives the closure authoritatively at install.
 *  Direct deps lead, transitive deps (revealed only by catalogued parents)
 *  follow. `installedKeys` = `${kind}:${name}` of the local store. */
export function resolveCatalogClosure(
	target: PrimitiveCatalogEntry,
	catalog: readonly PrimitiveCatalogEntry[],
	installedKeys: ReadonlySet<string>
): ConsentDep[] {
	const catByKey = new Map<string, PrimitiveCatalogEntry>();
	for (const c of catalog) catByKey.set(`${c.kind}:${c.name}`, c);

	const out: ConsentDep[] = [];
	const visited = new Set<string>([`${target.kind}:${target.name}`]);
	const queue: RequiresEntry[] = [...(target.requires ?? [])];

	while (queue.length) {
		const r = queue.shift() as RequiresEntry;
		const key = `${r.kind}:${r.name}`;
		if (visited.has(key)) continue;
		visited.add(key);

		const cat = catByKey.get(key) ?? null;
		let resolution: ConsentResolution;
		let provenance: string;
		if (cat) {
			resolution = 'catalog';
			provenance = `${cat.source} · ${cat.url}`;
			// A catalogued dep can reveal its own deps (transitive closure).
			for (const child of cat.requires ?? []) queue.push(child);
		} else if (r.source) {
			resolution = 'pinned';
			provenance = `${r.source}${r.ref ? ` @ ${r.ref}` : ''} · not in catalog`;
		} else {
			resolution = 'unresolved';
			provenance = 'unresolved — not in catalog and no pinned source';
		}

		out.push({
			kind: r.kind as ClaudeStoreKind,
			name: r.name,
			resolution,
			provenance,
			satisfied: installedKeys.has(key),
			needsExtraConfirm: resolution !== 'catalog',
		});
	}
	return out;
}
