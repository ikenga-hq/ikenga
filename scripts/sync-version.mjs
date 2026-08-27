#!/usr/bin/env node
// Propagate the package.json version (bumped by `changeset version`) into the
// other places Tauri and the daemon read it: src-tauri/tauri.conf.json and Cargo.toml,
// plus the ikenga-desktop entry in Cargo.lock. Run via `bun run changeset:version`.
//
// This is the "3-file problem" that made manual shell version bumps error-prone
// (see the 0.1.1 → 0.2.0 release). Changesets owns package.json; this keeps the
// rest in lockstep.
import { readFileSync, writeFileSync } from 'node:fs';

const version = JSON.parse(readFileSync('package.json', 'utf8')).version;
if (!version) {
	console.error('sync-version: no version in package.json');
	process.exit(1);
}

const edits = [
	// tauri.conf.json — first top-level "version" field
	{
		path: 'src-tauri/tauri.conf.json',
		re: /("version":\s*")[^"]+(")/,
		label: 'tauri.conf.json',
	},
	// Cargo.toml — the [package] version is the first `version = "..."` line
	{
		path: 'src-tauri/Cargo.toml',
		re: /^(version\s*=\s*")[^"]+(")/m,
		label: 'Cargo.toml',
	},
	// Cargo.lock — the ikenga-desktop package entry
	{
		path: 'src-tauri/Cargo.lock',
		re: /(name = "ikenga-desktop"\nversion = ")[^"]+(")/,
		label: 'Cargo.lock',
	},
	// src-tauri/server/Cargo.toml — the headless daemon is its own crate (it
	// cannot be a second [[bin]] of the Tauri crate without being bundled into
	// every desktop artifact), so its version needs propagating too. This is
	// what `ikenga-server --version` reports.
	{
		path: 'src-tauri/server/Cargo.toml',
		re: /^(version\s*=\s*")[^"]+(")/m,
		label: 'server/Cargo.toml',
	},
	// Cargo.lock — the ikenga-server package entry
	{
		path: 'src-tauri/Cargo.lock',
		re: /(name = "ikenga-server"\nversion = ")[^"]+(")/,
		label: 'Cargo.lock (ikenga-server)',
	},
];

for (const { path, re, label } of edits) {
	const before = readFileSync(path, 'utf8');
	if (!re.test(before)) {
		console.error(`sync-version: pattern not found in ${label} (${path})`);
		process.exit(1);
	}
	const after = before.replace(re, `$1${version}$2`);
	if (after !== before) writeFileSync(path, after);
	console.log(`sync-version: ${label} → ${version}`);
}

console.log(`sync-version: done (${version})`);
