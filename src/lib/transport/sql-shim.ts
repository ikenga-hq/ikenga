import { getTransport, isTauri } from './index';

export interface SqlQueryResult {
	rowsAffected: number;
	lastInsertId: number;
}

export interface SqlDbInterface {
	select<T = unknown[]>(query: string, bindValues?: unknown[]): Promise<T>;
	execute(query: string, bindValues?: unknown[]): Promise<SqlQueryResult>;
}

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
