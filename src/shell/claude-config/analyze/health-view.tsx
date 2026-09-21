// Inventory & health (Phase 4 · D-03 "Health" view) — the Ngwa Analyze
// `surface=health` roll-up. Tallies the scan (`items: EngineConfigItem[]`) by
// kind × scope, lifecycle state, and mechanism, plus store utilization from the
// catalog (`store`). Rides the current scan; no Rust change. Per-view filters:
// scope (select) + kind (pills).
//
// Visual ported from `plans/cockpit/designs/cockpit-views-hifi.html` (renderHealth)
// against live data — Theme A · Dusk Wood.

import { useMemo, useState } from 'react';
import { cn } from '@/components/ui/utils';
import type { ClaudeStoreEntry, ClaudeStoreKind, ClaudeStoreScope } from '@/lib/tauri-cmd';
import type { ItemState, EngineConfigItem } from '../ngwa-surface';

interface HealthViewProps {
	items: EngineConfigItem[];
	store: ClaudeStoreEntry[];
	/** Sidebar scope — informational; the view carries its own scope select. */
	scope: string;
}

// KIND_ORDER deliberately omits `bundle` — the WP-18 schema kind has no live
// FE surface yet (bundles are filtered out of `claude_store_list` by the
// is_file_based guard; bundle listing arrives in a later WP). The label map
// stays exhaustive over ClaudeStoreKind so the union widening type-checks.
const KIND_ORDER: ClaudeStoreKind[] = ['skill', 'agent', 'command', 'hook', 'mcp'];
const KIND_LABEL: Record<ClaudeStoreKind, string> = {
	skill: 'Skills',
	agent: 'Agents',
	command: 'Commands',
	hook: 'Hooks',
	mcp: 'MCPs',
	bundle: 'Bundles',
};
const STATE_ORDER: ItemState[] = ['enabled', 'linked', 'disabled', 'local', 'orphaned'];
const STATE_LABEL: Record<ItemState, string> = {
	enabled: 'enabled',
	linked: 'linked',
	disabled: 'disabled',
	local: 'local',
	orphaned: 'orphaned',
};
/** Distinct hues for scope segments — cycles the kind palette so scopes read
 *  apart without inventing new tokens. */
const SCOPE_HUES = [
	'var(--nk-mcp)',
	'var(--nk-agent)',
	'var(--nk-skill)',
	'var(--nk-command)',
	'var(--nk-hook)',
];

interface Seg {
	key: string;
	label: string;
	count: number;
	color: string;
}
function Bar({ segs, total, height = 16 }: { segs: Seg[]; total: number; height?: number }) {
	return (
		<div className="ngwa-health-bar" style={{ height }}>
			{total === 0 ? (
				<span className="ngwa-health-bar-empty" />
			) : (
				segs
					.filter((s) => s.count > 0)
					.map((s) => (
						<span
							key={s.key}
							className="seg"
							style={{ width: `${(s.count / total) * 100}%`, background: s.color }}
							title={`${s.label}: ${s.count}`}
						/>
					))
			)}
		</div>
	);
}
function Legend({ segs }: { segs: Seg[] }) {
	return (
		<div className="ngwa-health-legend">
			{segs.map((s) => (
				<span key={s.key}>
					<i style={{ background: s.color }} />
					{s.label} {s.count}
				</span>
			))}
		</div>
	);
}

export function HealthView({ items, store }: HealthViewProps) {
	const [scopeSel, setScopeSel] = useState('all');
	const [hidden, setHidden] = useState<Set<ClaudeStoreKind>>(() => new Set());

	// Scope options from the items' own scope grammar (matches the store map).
	const scopeOptions = useMemo(() => {
		const m = new Map<string, string>();
		for (const it of items) m.set(it.scopeKey, it.scopeLabel);
		return [...m.entries()].sort((a, b) =>
			a[0] === 'workspace' ? -1 : b[0] === 'workspace' ? 1 : a[1].localeCompare(b[1])
		);
	}, [items]);
	const scopeColor = useMemo(() => {
		const m = new Map<string, string>();
		scopeOptions.forEach(([key], i) => {
			m.set(key, SCOPE_HUES[i % SCOPE_HUES.length]);
		});
		return m;
	}, [scopeOptions]);

	const kindsPresent = useMemo(
		() => KIND_ORDER.filter((k) => items.some((it) => it.storeKind === k)),
		[items]
	);

	// Scope + kind filtered working set.
	const view = useMemo(
		() =>
			items.filter(
				(it) => (scopeSel === 'all' || it.scopeKey === scopeSel) && !hidden.has(it.storeKind)
			),
		[items, scopeSel, hidden]
	);
	const storeView = useMemo(() => store.filter((e) => !hidden.has(e.kind)), [store, hidden]);

	// KPIs.
	const total = view.length;
	const scopeCount = scopeSel === 'all' ? scopeOptions.length : 1;
	const localCount = view.filter((it) => it.state === 'local').length;
	const orphanCount = view.filter((it) => it.state === 'orphaned').length;

	// Kind × scope: one row per kind, segments per scope.
	const kindRows = useMemo(
		() =>
			kindsPresent
				.filter((k) => !hidden.has(k))
				.map((k) => {
					const its = view.filter((it) => it.storeKind === k);
					const byScope = new Map<string, number>();
					for (const it of its) byScope.set(it.scopeKey, (byScope.get(it.scopeKey) ?? 0) + 1);
					const segs: Seg[] = [...byScope.entries()].map(([key, count]) => ({
						key,
						label: scopeOptions.find(([s]) => s === key)?.[1] ?? key,
						count,
						color: scopeColor.get(key) ?? 'var(--border-strong)',
					}));
					return { kind: k, total: its.length, segs };
				}),
		[kindsPresent, hidden, view, scopeOptions, scopeColor]
	);

	const stateSegs: Seg[] = STATE_ORDER.map((s) => ({
		key: s,
		label: STATE_LABEL[s],
		count: view.filter((it) => it.state === s).length,
		color: `var(--st-${s})`,
	}));
	const mechSegs: Seg[] = [
		{
			key: 'link',
			label: 'symlink',
			count: view.filter((it) => it.mech === 'link').length,
			color: 'var(--st-enabled)',
		},
		{
			key: 'merge',
			label: 'JSON-merge',
			count: view.filter((it) => it.mech === 'merge').length,
			color: 'var(--nk-mcp)',
		},
	];
	const linked = storeView.filter((e) =>
		scopeSel === 'all' ? e.enabledIn.length > 0 : e.enabledIn.includes(scopeSel as ClaudeStoreScope)
	).length;
	const storeSegs: Seg[] = [
		{ key: 'linked', label: 'linked into ≥1 scope', count: linked, color: 'var(--st-enabled)' },
		{
			key: 'unused',
			label: 'unused',
			count: storeView.length - linked,
			color: 'var(--border-strong)',
		},
	];

	const toggleKind = (k: ClaudeStoreKind) =>
		setHidden((prev) => {
			const next = new Set(prev);
			next.has(k) ? next.delete(k) : next.add(k);
			return next;
		});

	if (items.length === 0)
		return (
			<div className="ngwa-analyze-empty">
				Nothing scanned yet. Once agents, skills, commands, hooks, or MCPs are configured, their
				distribution and health show up here.
			</div>
		);

	return (
		<div className="ngwa-graph">
			<div className="ngwa-graph-toolbar">
				<div className="ngwa-graph-title">
					<span className="ngwa-graph-glyph">▥</span> Inventory &amp; health
				</div>
				<div className="ngwa-graph-meta">
					{total} primitives · {scopeCount} {scopeCount === 1 ? 'scope' : 'scopes'}
				</div>
				<div className="ngwa-graph-spacer" />
				<div className="ngwa-kind-filter">
					{kindsPresent.map((k) => (
						<button
							key={k}
							type="button"
							className={cn('ngwa-kf', hidden.has(k) && 'off')}
							onClick={() => toggleKind(k)}
							title={`Toggle ${KIND_LABEL[k]}`}
						>
							<span className="kd" style={{ background: `var(--nk-${k})` }} />
							{KIND_LABEL[k]}
						</button>
					))}
				</div>
				<select
					className="ngwa-graph-select"
					value={scopeSel}
					onChange={(e) => setScopeSel(e.target.value)}
					title="Narrow to one scope / project"
				>
					<option value="all">All scopes</option>
					{scopeOptions.map(([key, label]) => (
						<option key={key} value={key}>
							{label}
						</option>
					))}
				</select>
			</div>
			<div className="ngwa-graph-stage" style={{ overflow: 'auto', background: 'var(--bg-base)' }}>
				<div className="ngwa-health">
					<div className="ngwa-health-kpis">
						{[
							{ n: total, l: 'primitives' },
							{ n: scopeCount, l: scopeCount === 1 ? 'scope' : 'scopes' },
							{ n: storeView.length, l: 'store entries' },
							{ n: localCount, l: 'local · not in store', tone: localCount ? 'warn' : '' },
							{ n: orphanCount, l: 'orphaned', tone: orphanCount ? 'bad' : '' },
						].map((k) => (
							<div key={k.l} className={cn('ngwa-health-kpi', k.tone)}>
								<div className="n">{k.n}</div>
								<div className="l">{k.l}</div>
							</div>
						))}
					</div>

					<div className="ngwa-health-panel">
						<h3>Primitives by kind × scope</h3>
						{kindRows.map((r) => (
							<div key={r.kind} className="ngwa-health-barrow">
								<span className="bl">
									<span className="kd" style={{ background: `var(--nk-${r.kind})` }} />
									{KIND_LABEL[r.kind]}
								</span>
								<Bar segs={r.segs} total={r.total} />
								<span className="bn">{r.total}</span>
							</div>
						))}
						<Legend
							segs={scopeOptions.map(([key, label]) => ({
								key,
								label,
								count: view.filter((it) => it.scopeKey === key).length,
								color: scopeColor.get(key) ?? 'var(--border-strong)',
							}))}
						/>
					</div>

					<div className="ngwa-health-two">
						<div className="ngwa-health-panel">
							<h3>State</h3>
							<Bar segs={stateSegs} total={total} height={22} />
							<Legend segs={stateSegs} />
						</div>
						<div className="ngwa-health-panel">
							<h3>Mechanism</h3>
							<Bar segs={mechSegs} total={total} height={22} />
							<Legend segs={mechSegs} />
							<h3 style={{ marginTop: 'var(--space-5)' }}>Store utilization</h3>
							<Bar segs={storeSegs} total={storeView.length} height={22} />
							<Legend segs={storeSegs} />
						</div>
					</div>
				</div>
			</div>
		</div>
	);
}
