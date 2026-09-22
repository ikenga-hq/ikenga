// Shared store-map model (Phase 4 · D-07). Both the Presence matrix and the
// Flow rail consume this: store-backed file primitives × scopes, with each
// (entry, scope) cell carrying its on-disk state. Built from the EngineConfigItem scan
// model (`buildItems`) + the store catalog (`ClaudeStoreEntry.enabledIn`).

import type { ClaudeStoreEntry, ClaudeStoreKind } from '@/lib/tauri-cmd';
import type { ItemState, EngineConfigItem } from '../ngwa-surface';

/** Per-cell presence: a scan state, or 'none' when absent in that scope. */
export type Cell = ItemState | 'none';

export interface StoreRow {
	key: string; // `${kind}:${name}`
	kind: ClaudeStoreKind;
	name: string;
	inStore: boolean;
	cells: Map<string, Cell>; // scopeKey → cell
	status: ItemState; // row-level badge
}

export interface StoreColumn {
	key: string; // scope key ('workspace' | `project:<id>`)
	label: string;
}

export interface StoreModel {
	columns: StoreColumn[];
	rows: StoreRow[];
	groups: { kind: ClaudeStoreKind; rows: StoreRow[] }[];
	totals: {
		rows: number;
		scopes: number;
		enabled: number;
		local: number;
		orphaned: number;
		disabled: number;
		symlinks: number;
	};
}

export const STORE_KIND_GLYPH: Record<string, string> = { agent: '★', skill: '◆', command: '⌘' };
export const STORE_KIND_LABEL: Record<string, string> = {
	agent: 'Agents',
	skill: 'Skills',
	command: 'Commands',
};
// Only file-based (symlink-farm) kinds participate in the store map; hooks/mcps
// are JSON-merge primitives, not Ọba symlink entries.
export const STORE_FILE_KINDS: ClaudeStoreKind[] = ['skill', 'agent', 'command'];
const KIND_ORDER: ClaudeStoreKind[] = ['skill', 'agent', 'command'];

function rowStatus(cells: Map<string, Cell>, inStore: boolean): ItemState {
	const vals = [...cells.values()];
	if (vals.includes('enabled')) return 'enabled';
	if (vals.includes('orphaned')) return 'orphaned';
	if (vals.includes('local')) return 'local';
	return inStore ? 'disabled' : 'local';
}

export function buildStoreModel(items: EngineConfigItem[], store: ClaudeStoreEntry[]): StoreModel {
	const fileItems = items.filter((i) => STORE_FILE_KINDS.includes(i.storeKind));

	// Columns: every scope key seen in items + store.enabledIn, 'workspace' first.
	const labelByScope = new Map<string, string>();
	labelByScope.set('workspace', 'Personal');
	const scopeSet = new Set<string>(['workspace']);
	for (const it of fileItems) {
		scopeSet.add(it.scopeKey);
		labelByScope.set(it.scopeKey, it.scopeLabel);
	}
	for (const e of store) {
		if (!STORE_FILE_KINDS.includes(e.kind)) continue;
		for (const sc of e.enabledIn) scopeSet.add(sc);
	}
	const columnKeys = [...scopeSet].sort((a, b) => {
		if (a === 'workspace') return -1;
		if (b === 'workspace') return 1;
		return (labelByScope.get(a) ?? a).localeCompare(labelByScope.get(b) ?? b);
	});
	const columns: StoreColumn[] = columnKeys.map((key) => ({
		key,
		label: labelByScope.get(key) ?? key,
	}));

	// Rows keyed by kind:name.
	const rowMap = new Map<string, StoreRow>();
	const storeByKey = new Map<string, ClaudeStoreEntry>();
	for (const e of store)
		if (STORE_FILE_KINDS.includes(e.kind)) storeByKey.set(`${e.kind}:${e.name}`, e);

	const ensureRow = (kind: ClaudeStoreKind, name: string): StoreRow => {
		const key = `${kind}:${name}`;
		let r = rowMap.get(key);
		if (!r) {
			r = { key, kind, name, inStore: storeByKey.has(key), cells: new Map(), status: 'disabled' };
			rowMap.set(key, r);
		}
		return r;
	};

	for (const it of fileItems) {
		// item.state is enabled|local|orphaned for link items.
		ensureRow(it.storeKind, it.name).cells.set(it.scopeKey, it.state);
	}
	for (const e of store) {
		if (!STORE_FILE_KINDS.includes(e.kind)) continue;
		ensureRow(e.kind, e.name);
	}
	for (const r of rowMap.values()) {
		for (const c of columnKeys) if (!r.cells.has(c)) r.cells.set(c, 'none');
		r.status = rowStatus(r.cells, r.inStore);
	}

	const rows = [...rowMap.values()];
	return { columns, rows, ...assemble(rows, columns) };
}

/** Group rows by kind + tally totals — shared by build + filter. */
function assemble(rows: StoreRow[], columns: StoreColumn[]): Pick<StoreModel, 'groups' | 'totals'> {
	const groups = KIND_ORDER.map((kind) => ({
		kind,
		rows: rows.filter((r) => r.kind === kind).sort((a, b) => a.name.localeCompare(b.name)),
	})).filter((g) => g.rows.length > 0);

	let enabled = 0;
	let local = 0;
	let orphaned = 0;
	let disabled = 0;
	let symlinks = 0;
	for (const r of rows) {
		for (const v of r.cells.values()) {
			if (v === 'enabled') {
				enabled++;
				symlinks++;
			} else if (v === 'local') local++;
			else if (v === 'orphaned') orphaned++;
		}
		if (r.status === 'disabled') disabled++;
	}
	return {
		groups,
		totals: {
			rows: rows.length,
			scopes: columns.length,
			enabled,
			local,
			orphaned,
			disabled,
			symlinks,
		},
	};
}

/** Narrow a built model by free-text name query and/or a single scope key.
 *  Scope-narrowing keeps only that column and drops rows with no link there. */
export function filterStoreModel(
	model: StoreModel,
	opts: { query?: string; scope?: string | null }
): StoreModel {
	const q = (opts.query ?? '').trim().toLowerCase();
	const scope = opts.scope && opts.scope !== 'all' ? opts.scope : null;
	const columns = scope ? model.columns.filter((c) => c.key === scope) : model.columns;
	const colKeys = new Set(columns.map((c) => c.key));
	let rows = model.rows.map((r) => {
		const cells = new Map([...r.cells].filter(([k]) => colKeys.has(k)));
		return { ...r, cells, status: rowStatus(cells, r.inStore) };
	});
	if (q) rows = rows.filter((r) => r.name.toLowerCase().includes(q));
	if (scope) rows = rows.filter((r) => entryLinks(r).length > 0);
	return { columns, rows, ...assemble(rows, columns) };
}

/** Scope-linked rows for a single entry (cells that aren't 'none'). */
export function entryLinks(row: StoreRow): { scope: string; state: Cell }[] {
	const out: { scope: string; state: Cell }[] = [];
	for (const [scope, state] of row.cells) if (state !== 'none') out.push({ scope, state });
	return out;
}
