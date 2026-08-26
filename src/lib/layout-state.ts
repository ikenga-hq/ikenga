// Layout-state persistence helpers for the workspace shell.
//
// Primary store: the canonical app SQLite db via `loadAppDb()` (`layout_state`
// kv). Fallback: window.localStorage. The fallback exists because Database.load
// has been observed to silently never resolve in some launches — the
// terminal session-store uses the same pattern. localStorage keys are
// namespaced under `__lstate__:` to avoid collision with other writers.
//
// On every save we always write to localStorage first (synchronous, never
// fails on the hot path) and best-effort upgrade to SQLite. On load, we
// prefer SQLite and fall through to localStorage on any error or timeout.
// Net result: state is durable even when the SQL plugin is misbehaving.

import type Database from '@/lib/transport/sql-shim';
import { loadAppDb } from '@/lib/sql-db';
import { scopedLsPrefix, windowLabel } from '@/lib/window/window-context';

const SQL_TIMEOUT_MS = 1500;
// Window-namespaced (plans/multi-window WP-05). The primary `main` window keeps
// the bare `__lstate__:` prefix (existing shadows preserved); a detached window
// inserts its label (`__lstate__:detached-1:`) so the localStorage fallback —
// shared across all same-origin Tauri windows (research 03) — can't clobber the
// primary's layout shadow. The SQLite copy is window-scoped separately via the
// `scopedKey` label fold below.
const LS_PREFIX = scopedLsPrefix('__lstate__:');

let dbPromise: Promise<Database | null> | null = null;

function withTimeout<T>(p: Promise<T>, ms: number, label: string): Promise<T> {
	return new Promise((resolve, reject) => {
		const t = setTimeout(() => reject(new Error(`${label} timed out after ${ms}ms`)), ms);
		p.then(
			(v) => {
				clearTimeout(t);
				resolve(v);
			},
			(e) => {
				clearTimeout(t);
				reject(e);
			}
		);
	});
}

/** Returns the SQL Database, or null if the plugin is unavailable / hung. */
function getDb(): Promise<Database | null> {
	if (!dbPromise) {
		dbPromise = withTimeout(loadAppDb(), SQL_TIMEOUT_MS, 'loadAppDb(ikenga.db)')
			.then<Database | null>((db) => db)
			.catch((err) => {
				// eslint-disable-next-line no-console
				console.warn('[layout-state] sqlite unavailable, using localStorage', err);
				// Cache null so we don't retry every call.
				return null;
			});
	}
	return dbPromise;
}

function lsRead<T>(key: string): T | null {
	try {
		const raw = localStorage.getItem(LS_PREFIX + key);
		return raw == null ? null : (JSON.parse(raw) as T);
	} catch {
		return null;
	}
}

function lsWrite(key: string, value: unknown): void {
	try {
		localStorage.setItem(LS_PREFIX + key, JSON.stringify(value));
	} catch {
		// quota / disabled — drop silently.
	}
}

export async function loadLayoutState<T>(key: string, fallback: T): Promise<T> {
	const db = await getDb();
	if (db) {
		try {
			const rows = (await withTimeout(
				db.select('SELECT value FROM layout_state WHERE key = ? LIMIT 1', [key]),
				SQL_TIMEOUT_MS,
				'sqlite SELECT'
			)) as Array<{ value: string }>;
			if (rows.length > 0) return JSON.parse(rows[0].value) as T;
		} catch (err) {
			// eslint-disable-next-line no-console
			console.warn('[layout-state] load via sqlite failed, falling back to localStorage', {
				key,
				err,
			});
		}
	}
	const ls = lsRead<T>(key);
	return ls ?? fallback;
}

export async function saveLayoutState(key: string, value: unknown): Promise<void> {
	// Always persist to localStorage first — synchronous, can't hang.
	lsWrite(key, value);
	const db = await getDb();
	if (!db) return;
	try {
		await withTimeout(
			db.execute(
				'INSERT INTO layout_state (key, value, updated_at) VALUES (?, ?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at',
				[key, JSON.stringify(value), Date.now()]
			),
			SQL_TIMEOUT_MS,
			'sqlite INSERT'
		);
	} catch (err) {
		// eslint-disable-next-line no-console
		console.warn('[layout-state] save via sqlite failed (kept in localStorage)', { key, err });
	}
}

/** Tiny debounce — avoids pulling in lodash for this single use.
 *
 * Returns the debounced function plus a `flush` helper that fires any
 * pending call synchronously.
 */
export function debounce<A extends unknown[]>(
	fn: (...args: A) => void,
	delay: number
): ((...args: A) => void) & { flush: () => void } {
	let timer: ReturnType<typeof setTimeout> | null = null;
	let pending: A | null = null;
	const debounced = ((...args: A) => {
		if (timer) clearTimeout(timer);
		pending = args;
		timer = setTimeout(() => {
			timer = null;
			const a = pending!;
			pending = null;
			fn(...a);
		}, delay);
	}) as ((...args: A) => void) & { flush: () => void };
	debounced.flush = () => {
		if (timer) {
			clearTimeout(timer);
			timer = null;
		}
		if (pending) {
			const a = pending;
			pending = null;
			fn(...a);
		}
	};
	return debounced;
}

// ─── Project-scoped layout state ──────────────────────────────────────
//
// Phase 6 (projects-first-class) scopes pane tree, files-explorer, and
// workspace panel sizes per project by suffixing the kv key with
// `.${projectId}`. The `project_id` column on `layout_state` stays
// unused for these keys — using suffixed keys keeps load/save signatures
// trivial and means the localStorage fallback (which has no SQL schema)
// works unchanged.

export function scopedKey(key: string, projectId: string): string {
	// Window-scope folded in (plans/multi-window WP-05, G-03 — a KEY-STRING
	// suffix, NOT a new SQL column). The primary `main` window keeps the bare
	// `${key}.${projectId}` key so existing per-project layout state is
	// preserved; a detached window appends `@${label}` so its layout lands in
	// a distinct partition of the shared `layout_state` table.
	const base = `${key}.${projectId}`;
	const label = windowLabel();
	return label === 'main' ? base : `${base}@${label}`;
}

export function loadScopedLayoutState<T>(key: string, projectId: string, fallback: T): Promise<T> {
	return loadLayoutState(scopedKey(key, projectId), fallback);
}

export function saveScopedLayoutState(
	key: string,
	projectId: string,
	value: unknown
): Promise<void> {
	return saveLayoutState(scopedKey(key, projectId), value);
}

/**
 * One-time migration helper: if a scoped key has no value but a legacy
 * un-suffixed value exists, copy the legacy value into the scoped key
 * and return it. Used on first hydrate after a user upgrades from a
 * pre-Phase-6 build (where everything was stored under the un-suffixed
 * key). Subsequent loads find the scoped row directly.
 */
export async function migrateLegacyKey<T>(key: string, projectId: string, fallback: T): Promise<T> {
	const scoped = scopedKey(key, projectId);
	const sentinel = Symbol('missing');
	const existing = (await loadLayoutState<T | typeof sentinel>(scoped, sentinel)) as
		| T
		| typeof sentinel;
	if (existing !== sentinel) return existing as T;
	const legacy = (await loadLayoutState<T | typeof sentinel>(key, sentinel)) as T | typeof sentinel;
	if (legacy === sentinel) return fallback;
	// Copy under scoped key for next time; don't await — best-effort.
	void saveLayoutState(scoped, legacy);
	return legacy as T;
}

/**
 * Erase a scoped layout key. Used by the "Reset layout for this project"
 * surface. Deletes both the SQLite row and the localStorage shadow.
 */
export async function deleteScopedLayoutState(key: string, projectId: string): Promise<void> {
	const scoped = scopedKey(key, projectId);
	try {
		localStorage.removeItem(LS_PREFIX + scoped);
	} catch {
		// ignore
	}
	const db = await getDb();
	if (!db) return;
	try {
		await withTimeout(
			db.execute('DELETE FROM layout_state WHERE key = ?', [scoped]),
			SQL_TIMEOUT_MS,
			'sqlite DELETE'
		);
	} catch (err) {
		// eslint-disable-next-line no-console
		console.warn('[layout-state] delete via sqlite failed', { scoped, err });
	}
}
