#!/usr/bin/env bun
/**
 * ACL parity gate — every `#[tauri::command]` we register must be granted.
 *
 * Since tauri 2.11.2 the ACL gate in `tauri/src/webview/mod.rs` reads
 * `if (plugin_command.is_some() || has_app_acl_manifest || !is_local)`. The
 * shell's main window loads REMOTE content (`http://localhost:<viewer_port>/`,
 * so it is same-origin with the viewer-server), which makes `is_local` false
 * and therefore puts every one of the app's own commands behind the ACL. A
 * command that is registered in `generate_handler!` but missing from
 * `src-tauri/permissions/app-commands.toml` is rejected at runtime with
 * `Command <name> not allowed by ACL` — and nothing in `cargo check`,
 * `tsc --noEmit`, the Rust tests or the frontend tests notices. That is exactly
 * how ikenga#140 shipped in v0.8.0: the whole FE⇄backend channel was dead and
 * every gate was green.
 *
 * This script closes that hole cheaply: it parses both lists and asserts they
 * are the same set. It costs milliseconds and needs no display.
 *
 * Exit: 0 in sync, 1 drifted.
 */

import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const ROOT = join(import.meta.dir, '..');
const LIB_RS = join(ROOT, 'src-tauri', 'src', 'lib.rs');
const PERMS = join(ROOT, 'src-tauri', 'permissions', 'app-commands.toml');
const PERMISSION = 'allow-app-commands';

/** Command names inside `tauri::generate_handler![ … ]`, module paths stripped. */
function registeredCommands(): Set<string> {
	const src = readFileSync(LIB_RS, 'utf8');
	const start = src.indexOf('tauri::generate_handler![');
	if (start === -1) throw new Error(`no tauri::generate_handler! in ${LIB_RS}`);
	const end = src.indexOf('])', start);
	if (end === -1) throw new Error(`unterminated generate_handler! in ${LIB_RS}`);
	const block = src.slice(start, end);

	const out = new Set<string>();
	for (const raw of block.split('\n')) {
		// Strip line comments, then take `path::to::command,` → `command`.
		const line = raw.replace(/\/\/.*$/, '').trim();
		const m = /^((?:[A-Za-z_][A-Za-z0-9_]*::)*)([a-z_][a-z0-9_]*)\s*,$/.exec(line);
		if (m) out.add(m[2]);
	}
	return out;
}

/** `commands.allow = [ … ]` of the `allow-app-commands` permission. */
function grantedCommands(): Set<string> {
	const src = readFileSync(PERMS, 'utf8');
	const anchor = src.indexOf(`identifier = "${PERMISSION}"`);
	if (anchor === -1) throw new Error(`no ${PERMISSION} permission in ${PERMS}`);
	const listStart = src.indexOf('commands.allow = [', anchor);
	if (listStart === -1) throw new Error(`${PERMISSION} has no commands.allow list`);
	const listEnd = src.indexOf(']', listStart);
	if (listEnd === -1) throw new Error(`${PERMISSION} has an unterminated commands.allow list`);
	const body = src.slice(listStart, listEnd);

	const out = new Set<string>();
	for (const m of body.matchAll(/"([a-z_][a-z0-9_]*)"/g)) out.add(m[1]);
	return out;
}

const registered = registeredCommands();
const granted = grantedCommands();
if (registered.size === 0) {
	console.error('[acl-parity] FAIL: parsed zero registered commands — the parser has drifted');
	process.exit(1);
}

const missing = [...registered].filter((c) => !granted.has(c)).sort();
const extra = [...granted].filter((c) => !registered.has(c)).sort();

if (missing.length) {
	console.error(
		`[acl-parity] FAIL: ${missing.length} command(s) registered in generate_handler! but NOT granted ` +
			`by "${PERMISSION}" in src-tauri/permissions/app-commands.toml.\n` +
			'  Every one of these is rejected at runtime in a release build ' +
			'("Command <name> not allowed by ACL") — see ikenga#140.\n' +
			missing.map((c) => `    + ${c}`).join('\n')
	);
}
if (extra.length) {
	console.error(
		`[acl-parity] FAIL: ${extra.length} command(s) granted by "${PERMISSION}" but no longer registered. ` +
			'Drop them so the grant stays an accurate description of the surface.\n' +
			extra.map((c) => `    - ${c}`).join('\n')
	);
}
if (missing.length || extra.length) process.exit(1);

console.log(`[acl-parity] OK: ${registered.size} commands registered and granted`);
