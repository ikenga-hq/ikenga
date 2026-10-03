#!/usr/bin/env node
// Runs every docs producer in this folder and writes its output under
// docs/generated/. Wired into `changeset:version` so each release PR carries
// regenerated files. A producer that fails prints its errors and leaves the
// last committed output in place; the remaining producers still run.
//
//   node scripts/docs/generate.mjs [--check] [--soft]
//
// `--check` writes nothing and exits 1 when a committed output is out of date.
// `--soft` is for the release step: a failure prints a warning and the exit
// code stays 0, so docs output can never block a release.

import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { generateChangelog } from './changelog-json.mjs';
import { generateIykeRoutes } from './iyke-routes.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const check = process.argv.includes('--check');
const soft = process.argv.includes('--soft');

const producers = [
	{
		name: 'changelog',
		run: () => generateChangelog({ root, check }),
	},
	{
		name: 'iyke-routes',
		run: () => generateIykeRoutes({ root, check }),
	},
];

let failed = 0;
for (const p of producers) {
	const res = p.run();
	if (res.ok) {
		console.log(`docs: ${p.name}: ${res.changed ? 'wrote' : 'up to date'}`);
		continue;
	}
	failed += 1;
	if (res.errors.length === 0) console.error(`docs: ${p.name}: ${res.out} is out of date`);
	for (const e of res.errors) console.error(`docs: ${p.name}: ${e}`);
}
if (failed > 0 && soft) {
	console.error('warning: docs/generated not regenerated; the last committed files are unchanged');
	process.exit(0);
}
process.exit(failed > 0 ? 1 : 0);
