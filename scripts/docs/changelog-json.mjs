#!/usr/bin/env node
// Turns the Changesets-written CHANGELOG.md into a small JSON document that
// lists every released version with its bump level and how many entries it
// carries in each section. It copies no entry text: the notes stay in
// CHANGELOG.md, and the docs site reads only these counts.
//
//   node scripts/docs/changelog-json.mjs [--in CHANGELOG.md] [--out docs/generated/changelog.json]
//                                        [--package package.json]
//
// Exits 1, and writes nothing, when the file does not parse or its newest
// version is not the version in package.json.
//
// Dependency-free on purpose: it runs inside the version step, before any
// install is guaranteed.

import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

export const REPO = 'ikenga-hq/ikenga';
export const PRODUCER = 'scripts/docs/changelog-json.mjs';
export const SOURCE_PATH = 'CHANGELOG.md';

const SEMVER =
	/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;
const ENTRY = /^- (?:[0-9a-f]{7,40}: )?\S/;
const SECTIONS = {
	'Major Changes': 'major',
	'Minor Changes': 'minor',
	'Patch Changes': 'patch',
};
const BUMP_ORDER = ['major', 'minor', 'patch'];

/** Semver precedence: negative when a < b, 0 when equal, positive when a > b. */
export function compareSemver(a, b) {
	const pa = SEMVER.exec(a);
	const pb = SEMVER.exec(b);
	if (!pa || !pb) throw new Error(`not semver: ${pa ? b : a}`);
	for (let i = 1; i <= 3; i++) {
		const d = Number(pa[i]) - Number(pb[i]);
		if (d !== 0) return d;
	}
	const ra = pa[4];
	const rb = pb[4];
	if (ra === undefined && rb === undefined) return 0;
	if (ra === undefined) return 1;
	if (rb === undefined) return -1;
	const ia = ra.split('.');
	const ib = rb.split('.');
	for (let i = 0; i < Math.max(ia.length, ib.length); i++) {
		const x = ia[i];
		const y = ib[i];
		if (x === undefined) return -1;
		if (y === undefined) return 1;
		const nx = /^\d+$/.test(x);
		const ny = /^\d+$/.test(y);
		if (nx && ny) {
			const d = Number(x) - Number(y);
			if (d !== 0) return d;
		} else if (nx) return -1;
		else if (ny) return 1;
		else if (x !== y) return x < y ? -1 : 1;
	}
	return 0;
}

/**
 * Parse Changesets output.
 *
 * Rules:
 * - The `# <package>` heading names the package; any other text before the
 *   first version is an error.
 * - `## <semver>` opens a version (anything else is an error).
 * - `### Major Changes`, `### Minor Changes` and `### Patch Changes` open
 *   sections (any other `###` is an error).
 * - A column-0 line matching `- <hash>: <text>` or `- <text>` is one entry.
 *   Every other line up to the next entry or heading (blank, indented, a
 *   nested bullet, or column-0 prose) continues it and is not counted.
 * - Versions must be strictly descending, and each needs at least one entry.
 * - `bump` is the highest non-empty section.
 *
 * Returns `{ package, versions, errors }`. `errors` is a list of strings that
 * start with the line number; `versions` is newest first, as in the file.
 */
export function parseChangelog(text) {
	const errors = [];
	const versions = [];
	let pkg = null;
	let current = null; // the version being built
	let section = null; // 'major' | 'minor' | 'patch' | null
	let inEntry = false;

	const closeVersion = () => {
		if (!current) return;
		const total = current.counts.major + current.counts.minor + current.counts.patch;
		if (total === 0) errors.push(`line ${current.line}: version ${current.version} has no entries`);
		current.bump = BUMP_ORDER.find((k) => current.counts[k] > 0) ?? 'patch';
		versions.push(current);
		current = null;
	};

	const lines = String(text).split(/\r?\n/);
	for (let i = 0; i < lines.length; i++) {
		const line = lines[i];
		const n = i + 1;

		const h2 = /^## (.*)$/.exec(line);
		if (h2) {
			closeVersion();
			section = null;
			inEntry = false;
			const version = h2[1].trim();
			if (!SEMVER.test(version)) {
				errors.push(`line ${n}: version heading is not semver: ${version}`);
				continue;
			}
			const prev = versions[versions.length - 1];
			if (prev && compareSemver(version, prev.version) >= 0) {
				errors.push(
					`line ${n}: versions must be strictly descending: ${version} after ${prev.version}`
				);
			}
			current = {
				version,
				tag: `v${version}`,
				bump: 'patch',
				counts: { major: 0, minor: 0, patch: 0 },
				line: n,
			};
			continue;
		}

		const h3 = /^### (.*)$/.exec(line);
		if (h3) {
			inEntry = false;
			const name = h3[1].trim();
			if (!(name in SECTIONS)) {
				errors.push(`line ${n}: unknown section ${name}`);
				section = null;
			} else if (current) {
				section = SECTIONS[name];
			} else {
				errors.push(`line ${n}: section ${name} outside a version`);
				section = null;
			}
			continue;
		}

		const h1 = /^# (.*)$/.exec(line);
		if (h1 && !inEntry) {
			if (pkg === null && !current) pkg = h1[1].trim();
			continue;
		}

		if (ENTRY.test(line)) {
			if (current && section) {
				current.counts[section] += 1;
				inEntry = true;
			} else {
				errors.push(`line ${n}: entry outside a section`);
			}
			continue;
		}

		// Anything else continues the open entry. Outside an entry only blank
		// lines are allowed.
		if (!inEntry && line.trim() !== '') {
			errors.push(`line ${n}: text outside an entry`);
		}
	}
	closeVersion();

	if (versions.length === 0 && errors.length === 0) errors.push('no versions found');
	if (!pkg) errors.push('no "# <package>" heading found');

	return {
		package: pkg,
		versions: versions.map(({ line, ...v }) => v),
		errors,
	};
}

/**
 * Build the output document from CHANGELOG.md text.
 * `packageVersion` (from package.json) must equal the newest heading.
 * Returns `{ doc, errors }`; `doc` is null when there are errors.
 */
export function buildChangelogDocument(text, { packageVersion } = {}) {
	const parsed = parseChangelog(text);
	const errors = [...parsed.errors];
	const newest = parsed.versions[0]?.version;
	if (packageVersion !== undefined && newest !== undefined && newest !== packageVersion) {
		errors.push(`newest version ${newest} does not match package.json version ${packageVersion}`);
	}
	if (errors.length > 0) return { doc: null, errors };
	return {
		errors,
		doc: {
			$schemaVersion: 1,
			kind: 'ikenga.changelog',
			source: {
				repo: REPO,
				version: newest,
				producer: PRODUCER,
				path: SOURCE_PATH,
				package: parsed.package,
			},
			versions: parsed.versions,
		},
	};
}

/** Two-space JSON with a final newline, so a second run makes no diff. */
export function renderChangelogJson(doc) {
	return `${JSON.stringify(doc, null, 2)}\n`;
}

/**
 * Read, build and (unless `check`) write. Paths are resolved against `root`.
 * Returns `{ ok, errors, out, changed }`.
 */
export function generateChangelog({
	root = process.cwd(),
	inPath = 'CHANGELOG.md',
	outPath = 'docs/generated/changelog.json',
	packagePath = 'package.json',
	check = false,
} = {}) {
	const inFile = resolve(root, inPath);
	const outFile = resolve(root, outPath);
	let text;
	let packageVersion;
	try {
		text = readFileSync(inFile, 'utf8');
		packageVersion = JSON.parse(readFileSync(resolve(root, packagePath), 'utf8')).version;
	} catch (err) {
		return { ok: false, errors: [String(err.message ?? err)], out: outFile, changed: false };
	}
	const { doc, errors } = buildChangelogDocument(text, { packageVersion });
	if (!doc) return { ok: false, errors, out: outFile, changed: false };

	const next = renderChangelogJson(doc);
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
	const res = generateChangelog(opts);
	if (!res.ok && res.errors.length > 0) {
		for (const e of res.errors) console.error(`changelog-json: ${e}`);
		process.exit(1);
	}
	if (!res.ok) {
		console.error(`changelog-json: ${res.out} is out of date`);
		process.exit(1);
	}
	console.log(res.changed ? `wrote ${res.out}` : `${res.out} is up to date`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
	main();
}
