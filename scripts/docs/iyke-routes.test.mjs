// Run with: node --test "scripts/docs/*.test.mjs"
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import {
	buildRoutesDocument,
	extractRoutes,
	generateIykeRoutes,
	groupOf,
	renderRoutesJson,
	tokenize,
} from './iyke-routes.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, '..', '..');
const fixture = readFileSync(join(here, 'fixtures', 'iyke-routes-sample.rs'), 'utf8');

// ── The synthetic fixture ───────────────────────────────────────────────────

test('the fixture yields every route, with one entry per method', () => {
	const { routes, errors } = extractRoutes(fixture);
	assert.deepEqual(errors, []);
	assert.deepEqual(
		routes.map((r) => `${r.method} ${r.path}`),
		[
			'GET /iyke/items',
			'POST /iyke/items/add',
			'POST /iyke/items/remove',
			'DELETE /iyke/misc/ping',
			'GET /iyke/misc/ping',
			'ANY /iyke/misc/any',
			'PATCH /iyke/misc/patch',
			'PUT /iyke/misc/put',
			'POST /iyke/open/query',
			'GET /iyke/state',
			'GET /iyke/things/*thing_id',
			'POST /iyke/things/archive/all',
			'GET /iyke/things/export',
			'POST /iyke/things/export',
			'GET /iyke/things/preview/:thing_id',
			'POST /iyke/things/reset',
		].sort((a, b) => {
			// The same order the producer uses: path, then method.
			const [ma, pa] = a.split(' ');
			const [mb, pb] = b.split(' ');
			const order = ['GET', 'POST', 'PUT', 'PATCH', 'DELETE', 'ANY'];
			return (pa < pb ? -1 : pa > pb ? 1 : 0) || order.indexOf(ma) - order.indexOf(mb);
		})
	);
});

test('single-line, multi-line with a trailing comma, chained and path-qualified calls all attribute', () => {
	const { routes } = extractRoutes(fixture);
	const has = (m, p) => routes.some((r) => r.method === m && r.path === p);
	assert.ok(has('GET', '/iyke/things/archive/all') === false);
	assert.ok(has('POST', '/iyke/things/archive/all'), 'multi-line call with a trailing comma');
	assert.ok(
		has('GET', '/iyke/things/export') && has('POST', '/iyke/things/export'),
		'chained methods'
	);
	assert.ok(
		has('GET', '/iyke/misc/ping') && has('DELETE', '/iyke/misc/ping'),
		'path-qualified method'
	);
	assert.ok(has('GET', '/iyke/things/*thing_id'), 'wildcard segment');
	assert.ok(has('GET', '/iyke/things/preview/:thing_id'), 'parameter segment');
});

test('comments, other prefixes and prose strings produce no routes', () => {
	const { routes, errors } = extractRoutes(fixture);
	assert.deepEqual(errors, []);
	const paths = routes.map((r) => r.path);
	assert.ok(!paths.includes('/iyke/commented-out'));
	assert.ok(!paths.includes('/iyke/in-a-block-comment'));
	assert.ok(!paths.some((p) => p.startsWith('/other')));
	assert.ok(!paths.includes('/'));
});

test('the document groups by first segment, sorted, with the unstable label', () => {
	const { doc, errors } = buildRoutesDocument(fixture, { version: '1.2.3' });
	assert.deepEqual(errors, []);
	assert.equal(doc.$schemaVersion, 1);
	assert.equal(doc.kind, 'ikenga.iyke-routes');
	assert.equal(doc.stability, 'unstable');
	assert.deepEqual(doc.source, {
		repo: 'ikenga-hq/ikenga',
		version: '1.2.3',
		producer: 'scripts/docs/iyke-routes.mjs',
		path: 'src-tauri/src/iyke/server.rs',
	});
	assert.deepEqual(
		doc.groups.map((g) => [g.group, g.routes.length]),
		[
			['items', 3],
			['misc', 5],
			['open', 1],
			['state', 1],
			['things', 6],
		]
	);
	for (const g of doc.groups) {
		for (const r of g.routes) assert.deepEqual(Object.keys(r), ['method', 'path']);
	}
	const out = renderRoutesJson(doc);
	assert.ok(out.endsWith('}\n'));
	assert.equal(out, renderRoutesJson(JSON.parse(out)));
});

test('reordering routes in the source does not change the output', () => {
	const lines = fixture.split('\n');
	const first = lines.findIndex((l) => l.includes('.route("/iyke/items", '));
	assert.ok(first >= 0);
	const block = lines.slice(first, first + 3);
	assert.ok(block.every((l) => l.includes('.route("/iyke/items')));
	const shuffled = [
		...lines.slice(0, first),
		...[...block].reverse(),
		...lines.slice(first + 3),
	].join('\n');
	assert.notEqual(shuffled, fixture);
	const a = buildRoutesDocument(fixture, { version: '1.0.0' });
	const b = buildRoutesDocument(shuffled, { version: '1.0.0' });
	assert.deepEqual(b.errors, []);
	assert.deepEqual(b.doc, a.doc);
});

test('groupOf takes the first segment after /iyke/', () => {
	assert.equal(groupOf('/iyke/state'), 'state');
	assert.equal(groupOf('/iyke/things/archive/all'), 'things');
	assert.equal(groupOf('/iyke/things/*id'), 'things');
});

test('the tokenizer keeps strings, skips comments and tells a char literal from a lifetime', () => {
	const t = tokenize(
		'fn f<\'a>(x: &\'a str) { let c = \'"\'; // "/iyke/no"\n let s = "/iyke/yes"; /* "/iyke/no2" */ }'
	);
	const strs = t.filter((x) => x.t === 'str').map((x) => x.v);
	assert.deepEqual(strs, ['/iyke/yes']);
	assert.ok(t.some((x) => x.t === 'char' && x.v === "'\"'"));
});

// ── Failure: no silent drops ────────────────────────────────────────────────

test('a literal that no route call accounts for fails the run', () => {
	// Break one attribution: the literal stays in the file but is no longer in a .route( call.
	const broken = fixture.replace(
		'.route("/iyke/items/add", post(add_item))',
		'.rout("/iyke/items/add", post(add_item))'
	);
	assert.notEqual(broken, fixture);
	const { doc, errors } = buildRoutesDocument(broken, { version: '1.0.0' });
	assert.equal(doc, null);
	assert.deepEqual(errors, ['unattributed literal: /iyke/items/add']);
});

test('a route with no HTTP method fails the run', () => {
	const broken = fixture.replace(
		'.route("/iyke/misc/put", put(put_misc))',
		'.route("/iyke/misc/put", put_misc)'
	);
	const { doc, errors } = buildRoutesDocument(broken, { version: '1.0.0' });
	assert.equal(doc, null);
	assert.ok(
		errors.some((e) => e === 'route /iyke/misc/put: no HTTP method found'),
		errors.join('|')
	);
});

test('source with no routes is an error, not an empty document', () => {
	const { doc, errors } = buildRoutesDocument('fn main() {}', { version: '1.0.0' });
	assert.equal(doc, null);
	assert.deepEqual(errors, ['no routes found']);
});

test('an unterminated string is reported, not thrown', () => {
	const { errors } = extractRoutes('let s = "oops');
	assert.deepEqual(errors, ['unterminated string']);
});

// ── Files ───────────────────────────────────────────────────────────────────

function scratch() {
	const dir = mkdtempSync(join(tmpdir(), 'iyke-routes-'));
	mkdirSync(join(dir, 'src-tauri', 'src', 'iyke'), { recursive: true });
	writeFileSync(join(dir, 'src-tauri', 'src', 'iyke', 'server.rs'), fixture);
	writeFileSync(join(dir, 'package.json'), JSON.stringify({ version: '1.2.3' }));
	return dir;
}

test('a second run makes no diff, and a failed run leaves the last output alone', () => {
	const dir = scratch();
	try {
		const first = generateIykeRoutes({ root: dir });
		assert.equal(first.ok, true);
		const out = join(dir, 'docs', 'generated', 'iyke-routes.json');
		const bytes = readFileSync(out, 'utf8');
		assert.equal(generateIykeRoutes({ root: dir }).changed, false);
		assert.equal(readFileSync(out, 'utf8'), bytes);

		writeFileSync(
			join(dir, 'src-tauri', 'src', 'iyke', 'server.rs'),
			fixture.replace('.route("/iyke/state"', '.rout("/iyke/state"')
		);
		const res = generateIykeRoutes({ root: dir });
		assert.equal(res.ok, false);
		assert.deepEqual(res.errors, ['unattributed literal: /iyke/state']);
		assert.equal(readFileSync(out, 'utf8'), bytes);
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
});

// ── The real file, read in place and never copied ───────────────────────────

test('server.rs in this checkout has every /iyke/ literal attributed to a route', () => {
	const src = readFileSync(join(repoRoot, 'src-tauri', 'src', 'iyke', 'server.rs'), 'utf8');
	const { routes, errors } = extractRoutes(src);
	assert.deepEqual(errors, []);
	const literals = new Set(
		tokenize(src)
			.filter((t) => t.t === 'str' && t.v.startsWith('/iyke/'))
			.map((t) => t.v)
	);
	assert.ok(literals.size > 0);
	assert.equal(new Set(routes.map((r) => r.path)).size, literals.size);
	assert.ok(routes.length >= literals.size);
});
