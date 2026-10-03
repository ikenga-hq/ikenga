#!/usr/bin/env node
// Lists the routes of the iyke bridge (the local HTTP interface that
// `iyke` and agents use to drive a running shell), grouped by their first
// path segment. It READS src-tauri/src/iyke/server.rs and never edits it.
//
//   node scripts/docs/iyke-routes.mjs [--in src-tauri/src/iyke/server.rs]
//                                     [--out docs/generated/iyke-routes.json]
//                                     [--package package.json] [--check]
//
// The output is labelled unstable: routes may change in any release. It carries
// no descriptions, only methods and paths.
//
// Every `"/iyke/..."` string literal in the file must be attributed to a
// `.route(...)` call, or the run fails. A route that is simply missed would
// otherwise vanish from the docs without a sound.
//
// Dependency-free on purpose: it runs inside the version step.

import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

export const REPO = 'ikenga-hq/ikenga';
export const PRODUCER = 'scripts/docs/iyke-routes.mjs';
export const SOURCE_PATH = 'src-tauri/src/iyke/server.rs';

const METHODS = ['get', 'post', 'put', 'patch', 'delete', 'any'];
const METHOD_ORDER = ['GET', 'POST', 'PUT', 'PATCH', 'DELETE', 'ANY'];
const IDENT = /[A-Za-z0-9_]/;

// Split Rust source into tokens, dropping line comments and block comments.
// String literals keep their text. Tokens are
// `{ t: 'str' | 'ident' | 'punct' | 'char', v }`.
export function tokenize(src) {
	const tokens = [];
	let i = 0;
	const n = src.length;
	while (i < n) {
		const c = src[i];
		if (c === '/' && src[i + 1] === '/') {
			while (i < n && src[i] !== '\n') i++;
			continue;
		}
		if (c === '/' && src[i + 1] === '*') {
			let depth = 1;
			i += 2;
			while (i < n && depth > 0) {
				if (src[i] === '/' && src[i + 1] === '*') {
					depth++;
					i += 2;
				} else if (src[i] === '*' && src[i + 1] === '/') {
					depth--;
					i += 2;
				} else i++;
			}
			continue;
		}
		if (/\s/.test(c)) {
			i++;
			continue;
		}
		// Raw string: r"..." or r#"..."# (optionally prefixed with b).
		const raw = /^b?r(#*)"/.exec(src.slice(i, i + 40));
		if (raw) {
			const hashes = raw[1];
			const close = `"${hashes}`;
			const start = i + raw[0].length;
			const end = src.indexOf(close, start);
			if (end < 0) throw new Error('unterminated raw string');
			tokens.push({ t: 'str', v: src.slice(start, end) });
			i = end + close.length;
			continue;
		}
		if (c === '"' || (c === 'b' && src[i + 1] === '"')) {
			i += c === 'b' ? 2 : 1;
			let v = '';
			while (i < n && src[i] !== '"') {
				if (src[i] === '\\') {
					v += src[i] + (src[i + 1] ?? '');
					i += 2;
				} else v += src[i++];
			}
			if (i >= n) throw new Error('unterminated string');
			i++;
			tokens.push({ t: 'str', v });
			continue;
		}
		if (c === "'") {
			const ch = /^'(?:\\u\{[0-9a-fA-F_]+\}|\\.|[^\\'])'/.exec(src.slice(i, i + 16));
			if (ch) {
				tokens.push({ t: 'char', v: ch[0] });
				i += ch[0].length;
			} else {
				tokens.push({ t: 'punct', v: c }); // a lifetime tick
				i++;
			}
			continue;
		}
		if (IDENT.test(c)) {
			let j = i;
			while (j < n && IDENT.test(src[j])) j++;
			tokens.push({ t: 'ident', v: src.slice(i, j) });
			i = j;
			continue;
		}
		tokens.push({ t: 'punct', v: c });
		i++;
	}
	return tokens;
}

/** Index of the token that closes the `(` at `open`, or -1. */
function matchParen(tokens, open) {
	let depth = 0;
	for (let i = open; i < tokens.length; i++) {
		const t = tokens[i];
		if (t.t !== 'punct') continue;
		if (t.v === '(' || t.v === '[' || t.v === '{') depth++;
		else if (t.v === ')' || t.v === ']' || t.v === '}') {
			depth--;
			if (depth === 0) return i;
		}
	}
	return -1;
}

/**
 * HTTP methods of a method-router expression, such as `get(h)` or
 * `get(a).post(b)`. A method name counts only at the top level of the
 * expression and only when called: at the start, after `.`, or after `::`.
 */
function methodsOf(expr) {
	const found = [];
	let depth = 0;
	for (let i = 0; i < expr.length; i++) {
		const t = expr[i];
		if (t.t === 'punct') {
			if (t.v === '(' || t.v === '[' || t.v === '{') depth++;
			else if (t.v === ')' || t.v === ']' || t.v === '}') depth--;
			continue;
		}
		if (depth !== 0 || t.t !== 'ident' || !METHODS.includes(t.v)) continue;
		const next = expr[i + 1];
		if (!next || next.t !== 'punct' || next.v !== '(') continue;
		const prev = expr[i - 1];
		const prev2 = expr[i - 2];
		const ok =
			!prev ||
			(prev.t === 'punct' && prev.v === '.') ||
			(prev.t === 'punct' && prev.v === ':' && prev2?.t === 'punct' && prev2.v === ':');
		if (ok) found.push(t.v.toUpperCase());
	}
	return found;
}

/**
 * Extract the routes from Rust source.
 * Returns `{ routes: [{ method, path }], errors }`. `routes` has one entry per
 * method per path, sorted by path and then method.
 */
export function extractRoutes(src) {
	const errors = [];
	let tokens;
	try {
		tokens = tokenize(src);
	} catch (err) {
		return { routes: [], errors: [String(err.message)] };
	}

	const attributed = new Set();
	const routes = [];

	for (let i = 0; i + 3 < tokens.length; i++) {
		const [dot, name, open] = [tokens[i], tokens[i + 1], tokens[i + 2]];
		if (dot.v !== '.' || name.v !== 'route' || name.t !== 'ident' || open.v !== '(') continue;
		const close = matchParen(tokens, i + 2);
		if (close < 0) {
			errors.push('unbalanced parentheses in a .route( call');
			continue;
		}
		const pathTok = tokens[i + 3];
		if (pathTok.t !== 'str' || !pathTok.v.startsWith('/iyke/')) continue;
		const comma = tokens[i + 4];
		if (!comma || comma.v !== ',') {
			errors.push(`route ${pathTok.v}: expected a method router after the path`);
			continue;
		}
		const expr = tokens.slice(i + 5, close);
		const methods = methodsOf(expr);
		if (methods.length === 0) {
			errors.push(`route ${pathTok.v}: no HTTP method found`);
			continue;
		}
		attributed.add(pathTok.v);
		for (const m of new Set(methods)) routes.push({ method: m, path: pathTok.v });
	}

	// No silent drops: every /iyke/ literal must belong to a route above.
	const literals = new Set(
		tokens.filter((t) => t.t === 'str' && t.v.startsWith('/iyke/')).map((t) => t.v)
	);
	for (const lit of [...literals].sort()) {
		if (!attributed.has(lit)) errors.push(`unattributed literal: ${lit}`);
	}

	routes.sort(
		(a, b) =>
			(a.path < b.path ? -1 : a.path > b.path ? 1 : 0) ||
			METHOD_ORDER.indexOf(a.method) - METHOD_ORDER.indexOf(b.method)
	);
	return { routes, errors };
}

/** First path segment after `/iyke/`. */
export function groupOf(path) {
	return path.slice('/iyke/'.length).split('/')[0];
}

/** Group routes by first segment; groups and routes are sorted. */
export function groupRoutes(routes) {
	const byGroup = new Map();
	for (const r of routes) {
		const g = groupOf(r.path);
		if (!byGroup.has(g)) byGroup.set(g, []);
		byGroup.get(g).push({ method: r.method, path: r.path });
	}
	return [...byGroup.keys()].sort().map((group) => ({ group, routes: byGroup.get(group) }));
}

/** Build the output document. Returns `{ doc, errors }`; `doc` is null on error. */
export function buildRoutesDocument(src, { version }) {
	const { routes, errors } = extractRoutes(src);
	if (!version) errors.push('no version given');
	if (routes.length === 0 && errors.length === 0) errors.push('no routes found');
	if (errors.length > 0) return { doc: null, errors, routes };
	return {
		errors,
		routes,
		doc: {
			$schemaVersion: 1,
			kind: 'ikenga.iyke-routes',
			stability: 'unstable',
			source: { repo: REPO, version, producer: PRODUCER, path: SOURCE_PATH },
			groups: groupRoutes(routes),
		},
	};
}

/** Two-space JSON with a final newline, so a second run makes no diff. */
export function renderRoutesJson(doc) {
	return `${JSON.stringify(doc, null, 2)}\n`;
}

/** Read, build and (unless `check`) write. Returns `{ ok, errors, out, changed }`. */
export function generateIykeRoutes({
	root = process.cwd(),
	inPath = SOURCE_PATH,
	outPath = 'docs/generated/iyke-routes.json',
	packagePath = 'package.json',
	check = false,
} = {}) {
	const outFile = resolve(root, outPath);
	let src;
	let version;
	try {
		src = readFileSync(resolve(root, inPath), 'utf8');
		version = JSON.parse(readFileSync(resolve(root, packagePath), 'utf8')).version;
	} catch (err) {
		return { ok: false, errors: [String(err.message ?? err)], out: outFile, changed: false };
	}
	const { doc, errors } = buildRoutesDocument(src, { version });
	if (!doc) return { ok: false, errors, out: outFile, changed: false };

	const next = renderRoutesJson(doc);
	let prev = null;
	try {
		prev = readFileSync(outFile, 'utf8');
	} catch {
		// first run
	}
	const changed = prev !== next;
	if (!check && changed) {
		mkdirSync(dirname(outFile), { recursive: true });
		writeFileSync(outFile, next, 'utf8');
	}
	return { ok: !check || !changed, errors: [], out: outFile, changed };
}

function parseArgs(argv) {
	const opts = {};
	for (let i = 0; i < argv.length; i++) {
		const a = argv[i];
		if (a === '--in') opts.inPath = argv[++i];
		else if (a === '--out') opts.outPath = argv[++i];
		else if (a === '--package') opts.packagePath = argv[++i];
		else if (a === '--check') opts.check = true;
		else throw new Error(`unknown argument: ${a}`);
	}
	return opts;
}

function main() {
	let opts;
	try {
		opts = parseArgs(process.argv.slice(2));
	} catch (err) {
		console.error(err.message);
		process.exit(2);
	}
	const res = generateIykeRoutes(opts);
	if (!res.ok && res.errors.length > 0) {
		for (const e of res.errors) console.error(`iyke-routes: ${e}`);
		process.exit(1);
	}
	if (!res.ok) {
		console.error(`iyke-routes: ${res.out} is out of date`);
		process.exit(1);
	}
	console.log(res.changed ? `wrote ${res.out}` : `${res.out} is up to date`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
	main();
}
