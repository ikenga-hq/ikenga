// Canonical FE handle to the shell's local SQLite database.
//
// THE BUG THIS FIXES: `Database.load('sqlite:ikenga.db')` resolves the
// relative name against tauri-plugin-sql's app **config** dir
// (~/.config/<id>/ikenga.db on Linux), but the Rust side — `PaDb`
// (commands/db.rs `default_db_path` → `app_data_dir().join("ikenga.db")`),
// the migration runner, `host.dbQuery`, and the cron daemon — all use the app
// **data** dir (~/.local/share/<id>/ikenga.db). Those are different files in
// different directories. The plugin is registered with no migrations
// (`Builder::default().build()`, since the migration list moved to the custom
// runner), so the config-dir file it opened was an EMPTY db with no tables —
// every FE SQL read failed with "no such table: …" and silently fell back to
// localStorage (layout), or silently no-op'd (clear-data).
//
// FIX: resolve the absolute data-dir path and hand tauri-plugin-sql an
// ABSOLUTE connection string. The plugin's `path_mapper` builds the final path
// with `PathBuf::push`, and pushing an absolute path replaces the config-dir
// base — so `sqlite:<abs>/ikenga.db` opens the exact file the Rust side owns.
// `appDataDir()` (JS) and `app_data_dir()` (Rust) resolve to the same place.

import { appDataDir, join } from '@/lib/transport/path-shim';
import Database from '@/lib/transport/sql-shim';

let cached: Promise<Database> | null = null;

/**
 * Load the canonical app database (the same `ikenga.db` the Rust side uses).
 * Cached after the first successful load; a failed load clears the cache so a
 * later call can retry. Callers keep their own try/catch — this throws on
 * failure rather than masking it.
 */
export function loadAppDb(): Promise<Database> {
	if (!cached) {
		cached = (async () => {
			const dir = await appDataDir();
			const path = await join(dir, 'ikenga.db');
			return Database.load(`sqlite:${path}`);
		})().catch((e) => {
			// Clear so the next call can retry rather than returning a poisoned
			// rejected promise forever.
			cached = null;
			throw e;
		});
	}
	return cached;
}
