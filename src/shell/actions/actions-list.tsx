// D-06 Actions surface (WP-57): facet bar (Source · Placement · Scope) + list
// (icon, name, runs, placements, key, source) + detail pane. Same shape as
// Ngwa Installed (`shell/ngwa/ngwa-list.tsx`) — a faceted list synced to a
// detail pane — per `designs/actions.html`'s own header comment ("FORM COMES
// FROM").

import { useMemo, useRef, useState } from 'react';
import type { KeyboardEvent } from 'react';
import { Search, X } from 'lucide-react';
import { useNavigate } from '@tanstack/react-router';
import { bindingsFor, type ActionsScope, type EffectiveAction, type EffectiveModel } from '@/lib/actions/store';
import { ActionIcon } from './shared/action-icon';
import { Kbd } from './shared/kbd';
import { placementCategory, placementCategoryLabel, placementIndex } from './shared/menu-label';
import { runText } from './shared/run-label';
import { ActionDetail } from './action-detail';

type SourceFacet = 'builtin' | 'package' | 'yours';
type ScopeFacet = 'personal' | 'project' | 'shell';

interface Facets {
	source: SourceFacet | null;
	scope: ScopeFacet | null;
	placement: string | null; // a placementCategory(), or null (no filter)
	search: string;
}

const DEFAULT_FACETS: Facets = { source: null, scope: null, placement: null, search: '' };

const SOURCE_LABEL: Record<SourceFacet, string> = { builtin: 'Built-in', package: 'Package', yours: 'Yours' };

function sourceBucket(action: EffectiveAction): SourceFacet {
	if (action.source === 'builtin') return 'builtin';
	if (action.source === 'package') return 'package';
	return 'yours';
}

function scopeBucket(action: EffectiveAction): ScopeFacet {
	if (action.source === 'personal') return 'personal';
	if (action.source === 'project') return 'project';
	return 'shell'; // built-in and package actions ship with the shell, not a user scope
}

function matchesSource(action: EffectiveAction, facet: SourceFacet | null): boolean {
	return facet === null || sourceBucket(action) === facet;
}

function matchesScope(action: EffectiveAction, facet: ScopeFacet | null): boolean {
	return facet === null || scopeBucket(action) === facet;
}

function matchesPlacement(_action: EffectiveAction, facet: string | null, placements: readonly string[]): boolean {
	if (facet === null) return true;
	return placements.some((at) => placementCategory(at) === facet);
}

function matchesSearch(action: EffectiveAction, query: string): boolean {
	if (!query) return true;
	const q = query.toLowerCase();
	const text = `${action.name} ${action.id} ${action.description}`.toLowerCase();
	return text.includes(q);
}

export interface ActionsListSurfaceProps {
	model: EffectiveModel;
	scope: ActionsScope;
}

export function ActionsListSurface({ model, scope }: ActionsListSurfaceProps) {
	const navigate = useNavigate();
	const [facets, setFacets] = useState<Facets>(DEFAULT_FACETS);
	const [selectedId, setSelectedId] = useState<string | null>(null);
	const rowRefs = useRef<Array<HTMLButtonElement | null>>([]);

	const actions = model.actions;

	// Blocker 3: a built-in's own `placements` is always `[]` — every
	// placement (built-in, package, or user) is read from `model.menus`
	// instead, the one source of truth for menu membership.
	const placements = useMemo(() => placementIndex(model), [model]);
	const placementsFor = (id: string): readonly string[] => placements.get(id) ?? [];

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
		for (const [, menuIds] of placements) for (const menuId of menuIds) set.add(placementCategory(menuId));
		return Array.from(set).sort();
	}, [placements]);

	const counts = useMemo(() => {
		const matchExcept = (a: EffectiveAction, except: keyof Facets) => {
			if (except !== 'source' && !matchesSource(a, facets.source)) return false;
			if (except !== 'scope' && !matchesScope(a, facets.scope)) return false;
			if (except !== 'placement' && !matchesPlacement(a, facets.placement, placementsFor(a.id))) return false;
			if (except !== 'search' && !matchesSearch(a, facets.search)) return false;
			return true;
		};
		const source: Record<SourceFacet, number> = { builtin: 0, package: 0, yours: 0 };
		const scope: Record<ScopeFacet, number> = { personal: 0, project: 0, shell: 0 };
		const placement: Record<string, number> = {};
		for (const cat of placementCategories) placement[cat] = 0;
		for (const a of actions) {
			if (matchExcept(a, 'source')) source[sourceBucket(a)]++;
			if (matchExcept(a, 'scope')) scope[scopeBucket(a)]++;
			if (matchExcept(a, 'placement')) {
				for (const cat of new Set(placementsFor(a.id).map(placementCategory))) {
					placement[cat] = (placement[cat] ?? 0) + 1;
				}
			}
		}
		return { source, scope, placement };
	}, [actions, facets, placementCategories, placements]);

	const filtered = useMemo(
		() =>
			actions.filter(
				(a) =>
					matchesSource(a, facets.source) &&
					matchesScope(a, facets.scope) &&
					matchesPlacement(a, facets.placement, placementsFor(a.id)) &&
					matchesSearch(a, facets.search)
			),
		[actions, facets, placements]
	);

	const activeAction = useMemo(() => {
		if (filtered.length === 0) return null;
		if (!selectedId) return filtered[0];
		return filtered.find((a) => a.id === selectedId) ?? filtered[0];
	}, [filtered, selectedId]);

	const hasActiveFilters =
		facets.source !== null || facets.scope !== null || facets.placement !== null || facets.search.trim() !== '';

	function focusRow(index: number) {
		rowRefs.current[index]?.focus();
	}

	function onListKeyDown(e: KeyboardEvent<HTMLDivElement>) {
		if (filtered.length === 0) return;
		const current = Math.max(0, filtered.findIndex((a) => a.id === activeAction?.id));
		let next = current;
		if (e.key === 'ArrowDown') next = Math.min(filtered.length - 1, current + 1);
		else if (e.key === 'ArrowUp') next = Math.max(0, current - 1);
		else if (e.key === 'Home') next = 0;
		else if (e.key === 'End') next = filtered.length - 1;
		else return;
		e.preventDefault();
		setSelectedId(filtered[next].id);
		focusRow(next);
	}

	return (
		<>
			<div className="facetbar">
				<div className="frow2">
					<span className="flabel">Source</span>
					{(['builtin', 'package', 'yours'] as const).map((id) => {
						const on = facets.source === id;
						const cnt = counts.source[id] ?? 0;
						return (
							<button
								key={id}
								type="button"
								className={`chip ${on ? 'on' : ''}`}
								aria-pressed={on}
								disabled={cnt === 0 && !on}
								onClick={() => setFacets((f) => ({ ...f, source: on ? null : id }))}
							>
								{SOURCE_LABEL[id]} <span className="n">{cnt}</span>
							</button>
						);
					})}
				</div>
				<div className="frow2">
					<span className="flabel">Placement</span>
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
								onClick={() => setFacets((f) => ({ ...f, placement: on ? null : cat }))}
							>
								{placementCategoryLabel(cat)} <span className="n">{cnt}</span>
							</button>
						);
					})}
				</div>
				<div className="frow2">
					<span className="flabel">Scope</span>
					{(['personal', 'project', 'shell'] as const).map((id) => {
						const on = facets.scope === id;
						const cnt = counts.scope[id] ?? 0;
						return (
							<button
								key={id}
								type="button"
								className={`chip ${on ? 'on' : ''}`}
								aria-pressed={on}
								disabled={cnt === 0 && !on}
								onClick={() => setFacets((f) => ({ ...f, scope: on ? null : id }))}
							>
								{id === 'shell' ? 'Shell (built-in)' : id === 'personal' ? 'Personal' : 'Project'}{' '}
								<span className="n">{cnt}</span>
							</button>
						);
					})}
					<span className="toolsep" />
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
					{hasActiveFilters && (
						<button type="button" className="chip clear" onClick={() => setFacets(DEFAULT_FACETS)}>
							<X className="h-3 w-3" /> Clear
						</button>
					)}
				</div>
			</div>

			<div className="split">
				<div className="listcol">
					<div className="lhead">
						<span>Action</span>
						<span className="c-runs">Runs</span>
						<span className="c-places">Placements</span>
						<span className="c-key">Key</span>
						<span className="c-src">Source</span>
					</div>
					<div
						className="sc"
						style={{ flex: 1, minHeight: 0 }}
						role="listbox"
						aria-label="Actions"
						onKeyDown={onListKeyDown}
					>
						{filtered.length === 0 && <div className="empty">Nothing matches these facets.</div>}
						{filtered.map((action, index) => (
							<ActionRow
								key={action.id}
								setRef={(el) => {
									rowRefs.current[index] = el;
								}}
								action={action}
								keyCombo={keyById.get(action.id) ?? null}
								places={placementsFor(action.id)}
								isSelected={activeAction?.id === action.id}
								onSelect={() => setSelectedId(action.id)}
							/>
						))}
					</div>
				</div>

				{activeAction ? (
					<ActionDetail
						action={activeAction}
						placements={placementsFor(activeAction.id)}
						scope={scope}
						projectId={scope === 'project' ? model.projectId : null}
						projectRoot={model.projectRoot}
						onTestRun={(id) =>
							void navigate({ to: '/settings/actions/$tab', params: { tab: 'editor' }, search: { action: id } })
						}
						onRebind={(id) =>
							void navigate({ to: '/settings/actions/$tab', params: { tab: 'keys' }, search: { action: id } })
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
	places,
	isSelected,
	onSelect,
	setRef,
}: {
	action: EffectiveAction;
	keyCombo: string | null;
	places: readonly string[];
	isSelected: boolean;
	onSelect: () => void;
	setRef: (el: HTMLButtonElement | null) => void;
}) {
	const shown = places.slice(0, 2);
	const more = places.length - shown.length;
	const sourceLabel =
		action.source === 'builtin' ? 'Built-in' : action.source === 'package' ? 'Package' : 'Yours';

	return (
		<button
			ref={setRef}
			type="button"
			role="option"
			aria-selected={isSelected}
			tabIndex={isSelected ? 0 : -1}
			data-id={action.id}
			data-source={action.source}
			className={`irow ${isSelected ? 'sel' : ''}`}
			onClick={onSelect}
		>
			<ActionIcon icon={action.icon} className="h-3.5 w-3.5 shrink-0" />
			<span className="nm">{action.name}</span>
			<span className="tail">
				<span className="c-runs">{runText(action)}</span>
				<span className="c-places">
					{shown.map((at) => (
						<span key={at} className="pchip">
							{placementCategoryLabel(placementCategory(at))}
						</span>
					))}
					{more > 0 && <span className="pchip more">+{more}</span>}
					{places.length === 0 && <span className="pchip none">—</span>}
				</span>
				<span className="c-key">
					<Kbd combo={keyCombo} />
				</span>
				<span className={`kind k-${action.source} c-src`}>
					<span className="src-dot" aria-hidden="true" />
					{sourceLabel}
				</span>
			</span>
		</button>
	);
}
