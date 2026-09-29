/**
 * R57 · Q3/Q8 — the pure half of the primitive-catalog pin generator.
 *
 * NODE-ONLY (node:fs / node:crypto): imported by `scripts/primitives-catalog-pin.ts`
 * and by its golden test, never by app code.
 *
 * Mirrors the Rust installer so a pin the generator writes is a pin the shell
 * verifies:
 *   - `contentHash*` ≙ `claude_store/source.rs::content_hash_*` (golden-tested
 *     on both sides against the same tree);
 *   - `locatePrimitive` ≙ `install.rs::locate_in_clone` + `source::locate_fragment`
 *     (the same bounded search, in the same order);
 *   - `liftRequires` ≙ `install.rs::requires_for` (root `manifest.json` first,
 *     then beside the located skill).
 */

import { createHash } from 'node:crypto';
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';

export type PrimitiveKind = 'skill' | 'agent' | 'command' | 'hook' | 'mcp';

export interface RequiresEntry {
	kind: string;
	name: string;
	source?: 'git' | 'npx' | 'catalog' | 'local';
	ref?: string;
}

const sha256hex = (b: Uint8Array | string) => createHash('sha256').update(b).digest('hex');

/** UTF-8 byte order — what Rust's `str::as_bytes().cmp` sorts by. */
function byteCompare(a: string, b: string): number {
	return Buffer.compare(Buffer.from(a, 'utf8'), Buffer.from(b, 'utf8'));
}

/**
 * sha256 over the sorted `(rel, bytes)` list:
 *   H.update(rel) ; H.update("\0") ; H.update(hex(sha256(bytes))) ; H.update("\n")
 * → `sha256-<hex>`. A single-file primitive hashes one entry with `rel = ""`.
 */
export function hashEntries(entries: Array<[string, Uint8Array]>): string {
	const sorted = [...entries].sort((a, b) => byteCompare(a[0], b[0]));
	const h = createHash('sha256');
	for (const [rel, bytes] of sorted) {
		h.update(Buffer.from(rel, 'utf8'));
		h.update(Buffer.from([0]));
		h.update(sha256hex(bytes));
		h.update('\n');
	}
	return `sha256-${h.digest('hex')}`;
}

export function contentHashBytes(bytes: Uint8Array): string {
	return hashEntries([['', bytes]]);
}

/** Every regular file under `dir` (following symlinks, skipping `.git`), as
 *  `['/'-relative path, absolute path]`, byte-sorted. */
export function walkFiles(dir: string): Array<[string, string]> {
	const out: Array<[string, string]> = [];
	const walk = (cur: string, prefix: string) => {
		for (const name of readdirSync(cur)) {
			if (name === '.git') continue;
			const abs = join(cur, name);
			const st = statSync(abs);
			const rel = prefix ? `${prefix}/${name}` : name;
			if (st.isDirectory()) walk(abs, rel);
			else if (st.isFile()) out.push([rel, abs]);
		}
	};
	walk(dir, '');
	return out.sort((a, b) => byteCompare(a[0], b[0]));
}

export function contentHashDir(dir: string): string {
	return hashEntries(walkFiles(dir).map(([rel, abs]) => [rel, readFileSync(abs)]));
}

/** Recursively key-sorted copy — serde_json's `Value` (no `preserve_order`)
 *  serializes objects with sorted keys, so an MCP def the Rust side extracts
 *  from a `{mcpServers}` wrapper is written key-sorted. */
export function sortKeysDeep(v: unknown): unknown {
	if (Array.isArray(v)) return v.map(sortKeysDeep);
	if (v && typeof v === 'object') {
		const o = v as Record<string, unknown>;
		return Object.fromEntries(
			Object.keys(o)
				.sort(byteCompare)
				.map((k) => [k, sortKeysDeep(o[k])])
		);
	}
	return v;
}

export type Located =
	| { form: 'dir'; path: string }
	| { form: 'file'; path: string }
	| { form: 'fragment'; bytes: Uint8Array; path: string };

const isFile = (p: string) => existsSync(p) && statSync(p).isFile();

/** Locate `kind`/`name` in a fetched tree — the installer's search, in order. */
export function locatePrimitive(root: string, kind: PrimitiveKind, name: string): Located | null {
	if (kind === 'skill') {
		if (isFile(join(root, 'SKILL.md'))) return { form: 'dir', path: root };
		for (const cand of [
			join(root, 'skills', name),
			join(root, name),
			join(root, '.claude', 'skills', name),
			join(root, '.agents', 'skills', name),
		]) {
			if (isFile(join(cand, 'SKILL.md'))) return { form: 'dir', path: cand };
		}
		return null;
	}
	if (kind === 'agent' || kind === 'command') {
		const sub = `${kind}s`;
		const leaf = `${name}.md`;
		for (const cand of [join(root, leaf), join(root, sub, leaf), join(root, '.claude', sub, leaf)]) {
			if (isFile(cand)) return { form: 'file', path: cand };
		}
		return null;
	}
	const sub = kind === 'hook' ? 'hooks' : 'mcp';
	const leaf = `${name}.json`;
	for (const cand of [join(root, sub, leaf), join(root, leaf)]) {
		if (isFile(cand)) {
			const bytes = readFileSync(cand);
			if (kind === 'mcp') {
				const v = JSON.parse(bytes.toString('utf8')) as Record<string, unknown>;
				const servers = v.mcpServers as Record<string, unknown> | undefined;
				if (servers && typeof servers === 'object') {
					if (!(name in servers)) return null;
					return { form: 'fragment', path: cand, bytes: prettyDef(servers[name]) };
				}
			}
			return { form: 'fragment', path: cand, bytes };
		}
	}
	if (kind === 'mcp' && isFile(join(root, '.mcp.json'))) {
		const v = JSON.parse(readFileSync(join(root, '.mcp.json'), 'utf8')) as {
			mcpServers?: Record<string, unknown>;
		};
		const def = v.mcpServers?.[name];
		if (def !== undefined) {
			return { form: 'fragment', path: join(root, '.mcp.json'), bytes: prettyDef(def) };
		}
	}
	return null;
}

/** `serde_json::to_vec_pretty` of a key-sorted value (2-space indent). */
function prettyDef(def: unknown): Uint8Array {
	return Buffer.from(JSON.stringify(sortKeysDeep(def), null, 2), 'utf8');
}

export function hashLocated(loc: Located): string {
	switch (loc.form) {
		case 'dir':
			return contentHashDir(loc.path);
		case 'file':
			return contentHashBytes(readFileSync(loc.path));
		case 'fragment':
			return contentHashBytes(loc.bytes);
	}
}

function readRequires(dir: string): RequiresEntry[] {
	const p = join(dir, 'manifest.json');
	if (!isFile(p)) return [];
	try {
		const m = JSON.parse(readFileSync(p, 'utf8')) as { requires?: unknown };
		return Array.isArray(m.requires) ? (m.requires as RequiresEntry[]) : [];
	} catch {
		return [];
	}
}

/** The compiled `requires` a fetched primitive declares (Q8): the tree root's
 *  `manifest.json`, else the located skill dir's. */
export function liftRequires(root: string, loc: Located): RequiresEntry[] {
	const atRoot = readRequires(root);
	if (atRoot.length || loc.form !== 'dir') return atRoot;
	return readRequires(loc.path);
}

export const looksLikeSha = (s: string) => /^[0-9a-f]{7,40}$/i.test(s);
