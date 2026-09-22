// Store map (Phase 4 · D-07) — Presence matrix with filters (free-text entry
// search + a scope narrow). The Flow-rail alternate was dropped (the matrix is
// the scalable workhorse); the bipartite-arc concept survives only as the
// `designs/analyze-map-flow.html` reference mockup.

import { useMemo, useState } from 'react';
import type { ClaudeStoreEntry } from '@/lib/tauri-cmd';
import type { EngineConfigItem } from '../ngwa-surface';
import { StoreMatrix } from './store-matrix';
import { buildStoreModel, filterStoreModel } from './store-model';

interface StoreMapProps {
	items: EngineConfigItem[];
	store: ClaudeStoreEntry[];
}

export function StoreMap({ items, store }: StoreMapProps) {
	const [query, setQuery] = useState('');
	const [scope, setScope] = useState<string>('all');

	const full = useMemo(() => buildStoreModel(items, store), [items, store]);
	const model = useMemo(() => filterStoreModel(full, { query, scope }), [full, query, scope]);

	return (
		<div className="ngwa-graph">
			<div className="ngwa-graph-toolbar">
				<div className="ngwa-graph-title">
					<span className="ngwa-graph-glyph">▦</span> Store map
				</div>
				<div className="ngwa-graph-meta">
					{model.totals.rows} entries · {model.totals.symlinks} links
				</div>
				<input
					className="ngwa-graph-search"
					placeholder="filter entries…"
					value={query}
					onChange={(e) => setQuery(e.target.value)}
				/>
				<select
					className="ngwa-graph-select"
					value={scope}
					onChange={(e) => setScope(e.target.value)}
					title="Narrow to one scope / project"
				>
					<option value="all">All scopes</option>
					{full.columns.map((c) => (
						<option key={c.key} value={c.key}>
							{c.label}
						</option>
					))}
				</select>
				<div className="ngwa-graph-spacer" />
			</div>
			{/* The matrix is a semantic <table> with a <caption> (its accessible
			    name/description) — do NOT wrap it in role="img", which would hide
			    the table's rows/headers from assistive tech. */}
			<div className="ngwa-graph-stage" style={{ background: 'var(--bg-base)' }}>
				<StoreMatrix model={model} />
			</div>
		</div>
	);
}
