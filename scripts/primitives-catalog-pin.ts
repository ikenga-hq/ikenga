#!/usr/bin/env bun
/**
 * R57 · Q3/Q8 — pin the Ọba primitive catalog (`primitives.json`).
 *
 *   bun run scripts/primitives-catalog-pin.ts <in primitives.json> <out.json> [--strict]
 *
 * For every entry it resolves the source's current commit (`git ls-remote`,
 * or re-verifies an existing SHA `ref`), fetches exactly that commit into a
 * temp dir, locates the primitive the way the shell's installer does, and
 * writes back:
 *   - `ref`      — the commit SHA (the pin the signature now covers: WHAT, not
 *                  only WHERE; catalog installs move only when this moves);
 *   - `hash`     — the content hash the installer verifies (`sha256-<hex>`);
 *   - `requires` — lifted from the fetched `manifest.json` (omitted if none).
 *
 * An entry that cannot be resolved (private / missing repo, primitive not
 * found) is left exactly as it was — unpinned — and reported; `--strict` makes
 * that a non-zero exit. Nothing is signed or published: the output is the
 * UNSIGNED file to review, sign with minisign and publish by hand.
 *
 * Public sources only (Q9): git runs with prompts disabled.
 */

import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import {
	hashLocated,
	liftRequires,
	locatePrimitive,
	looksLikeSha,
	type PrimitiveKind,
} from './catalog-pin/core';

interface Entry {
	kind: PrimitiveKind;
	name: string;
	source: 'git' | 'npx';
	url: string;
	ref?: string;
	hash?: string;
	requires?: unknown[];
	[k: string]: unknown;
}

const GIT_ENV = { ...process.env, GIT_TERMINAL_PROMPT: '0', GCM_INTERACTIVE: 'never' };

function git(args: string[], cwd?: string): string {
	const r = spawnSync('git', args, { cwd, env: GIT_ENV, encoding: 'utf8' });
	if (r.status !== 0) throw new Error((r.stderr || r.stdout || `git ${args[0]} failed`).trim());
	return r.stdout.trim();
}

const cloneUrl = (e: Entry) =>
	e.source === 'npx' ? `https://github.com/${e.url.replace(/^github:/, '')}` : e.url;

function resolveSha(url: string, ref?: string): string {
	if (ref && looksLikeSha(ref)) return ref.toLowerCase();
	const line = git(['ls-remote', url, ref || 'HEAD']).split('\n')[0] ?? '';
	const sha = line.split(/\s+/)[0];
	if (!sha) throw new Error(`ls-remote returned no ref for ${url} ${ref || 'HEAD'}`);
	return sha;
}

function fetchAt(url: string, sha: string, dest: string): void {
	git(['init', '-q', dest]);
	git(['-C', dest, 'config', 'core.autocrlf', 'false']);
	git(['-C', dest, 'fetch', '-q', '--depth', '1', url, sha]);
	git(['-C', dest, 'checkout', '-q', '--detach', 'FETCH_HEAD']);
	const head = git(['-C', dest, 'rev-parse', 'HEAD']);
	if (!head.startsWith(sha) && !sha.startsWith(head)) {
		throw new Error(`fetched ${head}, expected ${sha}`);
	}
}

function pinEntry(e: Entry): { entry: Entry; note: string } {
	const url = cloneUrl(e);
	const sha = resolveSha(url, e.ref);
	const dir = mkdtempSync(join(tmpdir(), 'oba-pin-'));
	try {
		fetchAt(url, sha, dir);
		const loc = locatePrimitive(dir, e.kind, e.name);
		if (!loc) throw new Error(`no ${e.kind} ${e.name} in ${url}@${sha.slice(0, 7)}`);
		const hash = hashLocated(loc);
		const requires = liftRequires(dir, loc);
		const { requires: _old, ...rest } = e;
		const entry: Entry = { ...rest, ref: sha, hash };
		if (requires.length) entry.requires = requires;
		return {
			entry,
			note: `pinned ${sha.slice(0, 7)} ${hash.slice(0, 17)}… requires=${requires.length}`,
		};
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
}

function main(): number {
	const args = process.argv.slice(2);
	const strict = args.includes('--strict');
	const [input, output] = args.filter((a) => !a.startsWith('--'));
	if (!input || !output) {
		console.error('usage: primitives-catalog-pin.ts <in primitives.json> <out.json> [--strict]');
		return 2;
	}
	const cat = JSON.parse(readFileSync(input, 'utf8')) as {
		primitives: Entry[];
		[k: string]: unknown;
	};
	const failures: string[] = [];
	const primitives = cat.primitives.map((e) => {
		try {
			const { entry, note } = pinEntry(e);
			console.log(`ok    ${e.kind}:${e.name}  ${note}`);
			return entry;
		} catch (err) {
			const msg = (err as Error).message.split('\n')[0];
			failures.push(`${e.kind}:${e.name} (${e.source} ${e.url}): ${msg}`);
			console.log(`SKIP  ${e.kind}:${e.name}  left unpinned — ${msg}`);
			return e;
		}
	});
	const out = { ...cat, updatedAt: new Date().toISOString(), primitives };
	writeFileSync(output, `${JSON.stringify(out, null, '\t')}\n`);
	console.log(`\nwrote ${output} — ${primitives.length - failures.length} pinned, ${failures.length} unpinned`);
	if (failures.length) console.log(`unpinned:\n  ${failures.join('\n  ')}`);
	return strict && failures.length ? 1 : 0;
}

process.exit(main());
