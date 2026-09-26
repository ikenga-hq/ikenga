// D-06 Actions surface (WP-57): facet bar (Source · Placement · Scope) + list
// (name, runs, key, source) + detail pane. Same shape as Ngwa Installed
// (`shell/ngwa/ngwa-list.tsx`) — a faceted list synced to a detail pane —
// per `designs/actions.html`'s own header comment ("FORM COMES FROM").

import { useMemo, useState } from 'react';
import { Search, X } from 'lucide-react';
import { useNavigate } from '@tanstack/react-router';
import { bindingsFor } from '@/lib/actions/store';
import type { EffectiveAction } from '@/lib/actions/registry';
import type { EffectiveModel } from '@/lib/actions/merge';
import { Kbd } from './shared/kbd';
import { placementCategory, placementCategoryLabel } from './shared/menu-label';
import { ActionDetail } from './action-detail';

type SourceFacet = 'all' | 'builtin' | 'package' | 'yours';
type ScopeFacet = 'all' | 'personal' | 'project';

interface Facets {
	source: SourceFacet;
	scope: ScopeFacet;
	placement: string; // 'all' or a placementCategory()
	search: string;
}

const DEFAULT_FACETS: Facets = { source: 'all', scope: 'all', placement: 'all', search: '' };

function sourceBucket(action: EffectiveAction): SourceFacet {
	if (action.source === 'builtin') return 'builtin';
	if (action.source === 'package') return 'package';
	return 'yours';
}

function matchesSource(action: EffectiveAction, facet: SourceFacet): boolean {
	return facet === 'all' || sourceBucket(action) === facet;
}

function matchesScope(action: EffectiveAction, facet: ScopeFacet): boolean {
	if (facet === 'all') return true;
	return action.source === facet;
}

function matchesPlacement(action: EffectiveAction, facet: string): boolean {
	if (facet === 'all') return true;
	return action.placements.some((p) => placementCategory(p.at) === facet);
}

function matchesSearch(action: EffectiveAction, query: string): boolean {
	if (!query) return true;
	const q = query.toLowerCase();
	const text = `${action.name} ${action.id} ${action.description}`.toLowerCase();
	return text.includes(q);
}

export interface ActionsListSurfaceProps {
	model: EffectiveModel;
}

export function ActionsListSurface({ model }: ActionsListSurfaceProps) {
	const navigate = useNavigate();
	const [facets, setFacets] = useState<Facets>(DEFAULT_FACETS);
	const [selectedId, setSelectedId] = useState<string | null>(null);

	const actions = model.actions;

	const keyById = useMemo(() => {
		const map = new Map<string, string | null>();
		for (const a of actions) {
			const bindings = bindingsFor(a.id);
			map.set(a.id, bindings.length > 0 ? bindings[0].key : null);
		}
		return map;
	}, [actions, model.keymap]);

	const placementCategories = useMemo(() => {
		const set = new Set<string>();
		for (const a of actions) for (const p of a.placements) set.add(placementCategory(p.at));
		return Array.from(set).sort();
	}, [actions]);

	const counts = useMemo(() => {
		const matchExcept = (a: EffectiveAction, except: keyof Facets) => {
			if (except !== 'source' && !matchesSource(a, facets.source)) return false;
			if (except !== 'scope' && !matchesScope(a, facets.scope)) return false;
			if (except !== 'placement' && !matchesPlacement(a, facets.placement)) return false;
			if (except !== 'search' && !matchesSearch(a, facets.search)) return false;
			return true;
		};
		const source: Record<string, number> = { all: 0, builtin: 0, package: 0, yours: 0 };
		const scopeCounts: Record<string, number> = { all: 0, personal: 0, project: 0 };
		const placement: Record<string, number> = { all: 0 };
		for (const cat of placementCategories) placement[cat] = 0;
		for (const a of actions) {
			if (matchExcept(a, 'source')) {
				source.all++;
				source[sourceBucket(a)]++;
			}
			if (matchExcept(a, 'scope')) {
				scopeCounts.all++;
				if (a.source === 'personal' || a.source === 'project') scopeCounts[a.source]++;
			}
			if (matchExcept(a, 'placement')) {
				placement.all++;
				for (const cat of new Set(a.placements.map((p) => placementCategory(p.at)))) {
					placement[cat] = (placement[cat] ?? 0) + 1;
				}
			}
		}
		return { source, scope: scopeCounts, placement };
	}, [actions, facets, placementCategories]);

	const filtered = useMemo(
		() =>
			actions.filter(
				(a) =>
					matchesSource(a, facets.source) &&
					matchesScope(a, facets.scope) &&
					matchesPlacement(a, facets.placement) &&
					matchesSearch(a, facets.search)
			),
		[actions, facets]
	);

	const activeAction = useMemo(() => {
		if (filtered.length === 0) return null;
		if (!selectedId) return filtered[0];
		return filtered.find((a) => a.id === selectedId) ?? filtered[0];
	}, [filtered, selectedId]);

	const hasActiveFilters =
		facets.source !== 'all' || facets.scope !== 'all' || facets.placement !== 'all' || facets.search.trim() !== '';

	return (
		<>
			<div className="facetbar">
				<div className="frow2">
					<span className="flabel">Source</span>
					{(['all', 'builtin', 'package', 'yours'] as const).map((id) => {
						const on = facets.source === id;
						const cnt = counts.source[id] ?? 0;
						return (
							<button
								key={id}
								type="button"
								className={`chip ${on ? 'on' : ''}`}
								aria-pressed={on}
								disabled={cnt === 0 && !on}
								onClick={() => setFacets((f) => ({ ...f, source: on && id !== 'all' ? 'all' : id }))}
							>
								{id === 'yours' ? 'yours' : id} <span className="n">{cnt}</span>
							</button>
						);
					})}
					<span className="toolsep" />
					<span className="flabel" style={{ width: 'auto' }}>
						Placement
					</span>
					<button
						type="button"
						className={`chip ${facets.placement === 'all' ? 'on' : ''}`}
						aria-pressed={facets.placement === 'all'}
						onClick={() => setFacets((f) => ({ ...f, placement: 'all' }))}
					>
						all <span className="n">{counts.placement.all ?? 0}</span>
					</button>
					{placementCategories.map((cat) => {
						const on = facets.placement === cat;
						const cnt = counts.placement[cat] ?? 0;
						return (
							<button
								key={cat}
								type="button"
								className={`chip ${on ? 'on' : ''}`}
								aria-pressed={on}
								disabled={cnt === 0 && !on}
								onClick={() => setFacets((f) => ({ ...f, placement: on ? 'all' : cat }))}
							>
								{placementCategoryLabel(cat)} <span className="n">{cnt}</span>
							</button>
						);
					})}
					<span className="toolsep" />
					<span className="flabel" style={{ width: 'auto' }}>
						Scope
					</span>
					{(['all', 'personal', 'project'] as const).map((id) => {
						const on = facets.scope === id;
						const cnt = counts.scope[id] ?? 0;
						return (
							<button
								key={id}
								type="button"
								className={`chip ${on ? 'on' : ''}`}
								aria-pressed={on}
								disabled={cnt === 0 && !on}
								onClick={() => setFacets((f) => ({ ...f, scope: on && id !== 'all' ? 'all' : id }))}
							>
								{id} <span className="n">{cnt}</span>
							</button>
						);
					})}
				</div>
				<div className="frow2">
					{hasActiveFilters && (
						<button type="button" className="chip clear" onClick={() => setFacets(DEFAULT_FACETS)}>
							<X className="h-3 w-3" /> Clear all
						</button>
					)}
					<div className="search">
						<Search className="h-3.5 w-3.5" />
						<input
							type="text"
							placeholder="Filter by name, id or description…"
							aria-label="Filter actions"
							value={facets.search}
							onChange={(e) => setFacets((f) => ({ ...f, search: e.target.value }))}
						/>
						{facets.search && (
							<button
								type="button"
								aria-label="Clear search"
								onClick={() => setFacets((f) => ({ ...f, search: '' }))}
							>
								<X className="h-3 w-3" />
							</button>
						)}
					</div>
				</div>
			</div>

			<div className="split">
				<div className="listcol">
					<div className="lhead">
						<span>Action</span>
						<span className="c-runs">Runs</span>
						<span className="c-key">Key</span>
						<span className="c-src">Source</span>
					</div>
					<div className="sc" style={{ flex: 1, minHeight: 0 }} role="listbox" aria-label="Actions" tabIndex={0}>
						{filtered.length === 0 && <div className="empty">Nothing matches these facets.</div>}
						{filtered.map((action) => (
							<ActionRow
								key={action.id}
								action={action}
								keyCombo={keyById.get(action.id) ?? null}
								isSelected={activeAction?.id === action.id}
								onSelect={() => setSelectedId(action.id)}
							/>
						))}
					</div>
				</div>

				{activeAction ? (
					<ActionDetail
						action={activeAction}
						onTestRun={(id) =>
							void navigate({ to: '/settings/actions/$tab', params: { tab: 'editor' }, search: { action: id } })
						}
					/>
				) : (
					<aside className="detailcol">
						<div className="empty">No action selected.</div>
					</aside>
				)}
			</div>
		</>
	);
}

function ActionRow({
	action,
	keyCombo,
	isSelected,
	onSelect,
}: {
	action: EffectiveAction;
	keyCombo: string | null;
	isSelected: boolean;
	onSelect: () => void;
}) {
	return (
		<button
			type="button"
			role="option"
			aria-selected={isSelected}
			data-id={action.id}
			data-source={action.source}
			className={`irow ${isSelected ? 'sel' : ''}`}
			onClick={onSelect}
		>
			<span className="nm">{action.name}</span>
			<span className="tail">
				<span className="c-runs">—</span>
				<span className="c-key">
					<Kbd combo={keyCombo} />
				</span>
				<span className={`kind k-${action.source} c-src`}>{action.source}</span>
			</span>
		</button>
	);
}
