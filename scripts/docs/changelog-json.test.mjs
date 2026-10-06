// Run with: node --test scripts/docs/
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync, existsSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import {
	buildChangelogDocument,
	compareSemver,
	generateChangelog,
	parseChangelog,
	renderChangelogJson,
} from './changelog-json.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, '..', '..');
const fixture = readFileSync(join(here, 'fixtures', 'changelog-sample.md'), 'utf8');

// ── The synthetic fixture ───────────────────────────────────────────────────

test('the fixture parses to three versions with the expected bump and counts', () => {
	const r = parseChangelog(fixture);
	assert.deepEqual(r.errors, []);
	assert.equal(r.package, 'ikenga-desktop');
	assert.deepEqual(r.versions, [
		{ version: '0.3.0', tag: 'v0.3.0', bump: 'minor', counts: { major: 0, minor: 1, patch: 3 } },
		{ version: '0.2.1', tag: 'v0.2.1', bump: 'patch', counts: { major: 0, minor: 0, patch: 1 } },
		{ version: '0.2.0', tag: 'v0.2.0', bump: 'minor', counts: { major: 0, minor: 1, patch: 1 } },
	]);
});

test('continuation lines never count: column-0 prose, nested bullets, indented paragraphs', () => {
	const r = parseChangelog(fixture);
	// 0.3.0 has a column-0 continuation and two nested bullets under one entry.
	assert.equal(r.versions[0].counts.minor, 1);
	// 0.2.1 has an indented continuation paragraph after its entry.
	assert.equal(r.versions[1].counts.patch, 1);
});

test('entries with and without a commit prefix both count', () => {
	const text = [
		'# pkg',
		'',
		'## 1.0.0',
		'',
		'### Patch Changes',
		'',
		'- abcdef0: prefixed',
		'- unprefixed',
		'- 0123456789abcdef0123456789abcdef01234567: long hash',
		'',
	].join('\n');
	assert.equal(parseChangelog(text).versions[0].counts.patch, 3);
});

test('a document built from the fixture has the contract shape and no entry text', () => {
	const { doc, errors } = buildChangelogDocument(fixture, { packageVersion: '0.3.0' });
	assert.deepEqual(errors, []);
	assert.deepEqual(Object.keys(doc), ['$schemaVersion', 'kind', 'source', 'versions']);
	assert.equal(doc.$schemaVersion, 1);
	assert.equal(doc.kind, 'ikenga.changelog');
	assert.deepEqual(doc.source, {
		repo: 'ikenga-hq/ikenga',
		version: '0.3.0',
		producer: 'scripts/docs/changelog-json.mjs',
		path: 'CHANGELOG.md',
		package: 'ikenga-desktop',
	});
	for (const v of doc.versions) {
		assert.deepEqual(Object.keys(v), ['version', 'tag', 'bump', 'counts']);
		assert.deepEqual(Object.keys(v.counts), ['major', 'minor', 'patch']);
	}
	const out = renderChangelogJson(doc);
	assert.ok(out.endsWith('}\n'));
	assert.equal(out, renderChangelogJson(JSON.parse(out)), 'rendering is stable');
	assert.ok(!/Example/.test(out), 'no entry text reaches the output');
});

test('bump is the highest non-empty section, including a major', () => {
	const text = [
		'# pkg',
		'',
		'## 2.0.0',
		'',
		'### Major Changes',
		'',
		'- aaaaaaa: breaking',
		'',
		'### Minor Changes',
		'',
		'- bbbbbbb: feature',
		'',
		'### Patch Changes',
		'',
		'- ccccccc: fix',
		'',
		'## 1.0.0',
		'',
		'### Patch Changes',
		'',
		'- ddddddd: fix',
	].join('\n');
	const r = parseChangelog(text);
	assert.deepEqual(r.errors, []);
	assert.equal(r.versions[0].bump, 'major');
	assert.deepEqual(r.versions[0].counts, { major: 1, minor: 1, patch: 1 });
	assert.equal(r.versions[1].bump, 'patch');
});

test('CRLF input parses the same as LF', () => {
	const a = parseChangelog(fixture);
	const b = parseChangelog(fixture.replace(/\n/g, '\r\n'));
	assert.deepEqual(b, a);
});

// ── Error cases ─────────────────────────────────────────────────────────────

const head = '# pkg\n\n';

test('a heading that is not semver is an error with its line number', () => {
	const r = parseChangelog(`${head}## 0.1.x\n\n### Patch Changes\n\n- aaaaaaa: x\n`);
	assert.deepEqual(r.errors.slice(0, 1), ['line 3: version heading is not semver: 0.1.x']);
});

test('an unknown section is an error', () => {
	const r = parseChangelog(`${head}## 0.1.0\n\n### Notes\n\n- aaaaaaa: x\n`);
	assert.ok(r.errors.includes('line 5: unknown section Notes'), r.errors.join('|'));
});

test('an entry outside a section is an error', () => {
	const r = parseChangelog(`${head}## 0.1.0\n\n- aaaaaaa: x\n`);
	assert.ok(
		r.errors.some((e) => e.startsWith('line 5: entry outside a section')),
		r.errors.join('|')
	);
});

test('an entry before any version is an error', () => {
	const r = parseChangelog('# pkg\n\n- aaaaaaa: x\n');
	assert.ok(
		r.errors.some((e) => e.startsWith('line 3: entry outside a section')),
		r.errors.join('|')
	);
});

test('versions out of order, or repeated, are errors', () => {
	const sec = '\n### Patch Changes\n\n- aaaaaaa: x\n';
	const up = parseChangelog(`${head}## 0.1.0\n${sec}\n## 0.2.0\n${sec}`);
	assert.ok(
		up.errors.some((e) => /strictly descending: 0\.2\.0 after 0\.1\.0/.test(e)),
		up.errors.join('|')
	);
	const dup = parseChangelog(`${head}## 0.1.0\n${sec}\n## 0.1.0\n${sec}`);
	assert.ok(
		dup.errors.some((e) => /strictly descending/.test(e)),
		dup.errors.join('|')
	);
});

test('a version with no entries is an error', () => {
	const r = parseChangelog(
		`${head}## 0.2.0\n\n### Patch Changes\n\n- aaaaaaa: x\n\n## 0.1.0\n\n### Patch Changes\n\n`
	);
	assert.ok(
		r.errors.some((e) => /version 0\.1\.0 has no entries/.test(e)),
		r.errors.join('|')
	);
});

test('prose between a version heading and its first section is an error, not a silent drop', () => {
	const r = parseChangelog(
		`${head}## 0.1.0\n\nSome stray text\n\n### Patch Changes\n\n- aaaaaaa: x\n`
	);
	assert.ok(
		r.errors.some((e) => e.startsWith('line 5: text outside an entry')),
		r.errors.join('|')
	);
});

test('an empty file, and a file with no package heading, are errors', () => {
	assert.deepEqual(parseChangelog('').errors, [
		'no versions found',
		'no "# <package>" heading found',
	]);
	const r = parseChangelog('## 0.1.0\n\n### Patch Changes\n\n- aaaaaaa: x\n');
	assert.deepEqual(r.errors, ['no "# <package>" heading found']);
});

test('the newest version must equal the package.json version', () => {
	const { doc, errors } = buildChangelogDocument(fixture, { packageVersion: '0.4.0' });
	assert.equal(doc, null);
	assert.deepEqual(errors, ['newest version 0.3.0 does not match package.json version 0.4.0']);
});

test('semver comparison orders releases and pre-releases', () => {
	assert.ok(compareSemver('1.0.0', '0.9.9') > 0);
	assert.ok(compareSemver('0.10.0', '0.9.0') > 0);
	assert.ok(compareSemver('1.0.0', '1.0.0-rc.1') > 0);
	assert.ok(compareSemver('1.0.0-rc.2', '1.0.0-rc.1') > 0);
	assert.equal(compareSemver('1.2.3', '1.2.3'), 0);
});

// ── Files: determinism, failure leaves the old output alone ─────────────────

function scratch() {
	const dir = mkdtempSync(join(tmpdir(), 'changelog-json-'));
	writeFileSync(join(dir, 'package.json'), JSON.stringify({ version: '0.3.0' }));
	writeFileSync(join(dir, 'CHANGELOG.md'), fixture);
	return dir;
}

test('a second run on the same input makes no diff', () => {
	const dir = scratch();
	try {
		const first = generateChangelog({ root: dir });
		assert.equal(first.ok, true);
		assert.equal(first.changed, true);
		const bytes = readFileSync(join(dir, 'docs', 'generated', 'changelog.json'), 'utf8');
		const second = generateChangelog({ root: dir });
		assert.equal(second.ok, true);
		assert.equal(second.changed, false);
		assert.equal(readFileSync(join(dir, 'docs', 'generated', 'changelog.json'), 'utf8'), bytes);
		assert.equal(generateChangelog({ root: dir, check: true }).ok, true);
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
});

test('--check reports a stale committed file and writes nothing', () => {
	const dir = scratch();
	try {
		mkdirSync(join(dir, 'docs', 'generated'), { recursive: true });
		writeFileSync(join(dir, 'docs', 'generated', 'changelog.json'), '{}\n');
		const res = generateChangelog({ root: dir, check: true });
		assert.equal(res.ok, false);
		assert.equal(readFileSync(join(dir, 'docs', 'generated', 'changelog.json'), 'utf8'), '{}\n');
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
});

test('a parse failure writes nothing and leaves the last output in place', () => {
	const dir = scratch();
	try {
		generateChangelog({ root: dir });
		const out = join(dir, 'docs', 'generated', 'changelog.json');
		const before = readFileSync(out, 'utf8');
		writeFileSync(join(dir, 'CHANGELOG.md'), fixture.replace('## 0.2.1', '## 0.2.x'));
		const res = generateChangelog({ root: dir });
		assert.equal(res.ok, false);
		assert.ok(res.errors.length > 0);
		assert.equal(readFileSync(out, 'utf8'), before);
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
});

test('the command line exits 1 on a mismatch and prints the reason', () => {
	const dir = scratch();
	try {
		writeFileSync(join(dir, 'package.json'), JSON.stringify({ version: '9.9.9' }));
		let err;
		try {
			execFileSync(process.execPath, [join(here, 'changelog-json.mjs')], {
				cwd: dir,
				stdio: 'pipe',
			});
		} catch (e) {
			err = e;
		}
		assert.ok(err, 'expected a non-zero exit');
		assert.equal(err.status, 1);
		assert.match(String(err.stderr), /does not match package\.json version 9\.9\.9/);
		assert.equal(existsSync(join(dir, 'docs', 'generated', 'changelog.json')), false);
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
});

// ── The real file, read in place and never copied ───────────────────────────

test('CHANGELOG.md in this checkout parses with no errors and matches package.json', () => {
	const text = readFileSync(join(repoRoot, 'CHANGELOG.md'), 'utf8');
	const pkg = JSON.parse(readFileSync(join(repoRoot, 'package.json'), 'utf8'));
	const { doc, errors } = buildChangelogDocument(text, { packageVersion: pkg.version });
	assert.deepEqual(errors, []);
	assert.equal(doc.source.version, pkg.version);
	assert.ok(doc.versions.length > 0);
});

test('the committed docs/generated/changelog.json is current', () => {
	const res = generateChangelog({ root: repoRoot, check: true });
	assert.equal(res.ok, true, 'run `node scripts/docs/generate.mjs` and commit the result');
});
