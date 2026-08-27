import { getTransport, isTauri } from './index';

export interface SqlQueryResult {
	rowsAffected: number;
	lastInsertId: number;
}

export interface SqlDbInterface {
	select<T = unknown[]>(query: string, bindValues?: unknown[]): Promise<T>;
	execute(query: string, bindValues?: unknown[]): Promise<SqlQueryResult>;
}

/**
 * Browser-side SQLite proxy.
 *
 * `db_query` / `db_exec` are served by the headless daemon as of WP-12b
 * (G-41, ikenga#100) — `server/rpc.rs` opens `<data_dir>/ikenga.db` through
 * the same `crate::db` module the Tauri commands use.
 *
 * ⚠ The argument names below are **not** the ones `tauri-cmd.ts` sends.
 * This class stands in for `@tauri-apps/plugin-sql`, so it keeps that
 * package's `{query, values}` spelling; `tauri-cmd.ts`'s `dbQuery`/`dbExec`
 * send `{sql, params}`. The daemon's `db_args` helper accepts both on
 * purpose. Renaming either side in isolation silently breaks half the app —
 * whichever half you are not looking at.
 *
 * If the daemon was started without `--data-dir` it has no database and both
 * calls reject with a message saying so. That degrades correctly: every
 * caller in the shell (`layout-state`, `session-store`, `sql-db`) treats a
 * rejected load as "SQL unavailable" and falls through to `localStorage`,
 * whereas an empty result would look like a real answer.
 */
export class SqlDbWebProxy implements SqlDbInterface {
	constructor(public readonly dbPath: string) {}

	async select<T = unknown[]>(query: string, bindValues: unknown[] = []): Promise<T> {
		const transport = getTransport();
		return transport.invoke<T>('db_query', { query, values: bindValues });
	}

	async execute(query: string, bindValues: unknown[] = []): Promise<SqlQueryResult> {
		const transport = getTransport();
		return transport.invoke<SqlQueryResult>('db_exec', { query, values: bindValues });
	}
}

export default class DatabaseShim implements SqlDbInterface {
	select<T = unknown[]>(_query: string, _bindValues?: unknown[]): Promise<T> {
		throw new Error('DatabaseShim instance should be obtained via DatabaseShim.load()');
	}

	execute(_query: string, _bindValues?: unknown[]): Promise<SqlQueryResult> {
		throw new Error('DatabaseShim instance should be obtained via DatabaseShim.load()');
	}

	static async load(path: string): Promise<SqlDbInterface> {
		if (isTauri()) {
			const mod = await import('@tauri-apps/plugin-sql');
			const RealDatabase = mod.default;
			return (await RealDatabase.load(path)) as unknown as SqlDbInterface;
		}
		return new SqlDbWebProxy(path);
	}

	static async get(path: string): Promise<SqlDbInterface> {
		return DatabaseShim.load(path);
	}
}
