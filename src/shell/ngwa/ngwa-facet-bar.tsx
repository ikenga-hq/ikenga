// Ngwa Facet Bar (WP-15 / locked D-02).
//
// Installed equipment facet bar:
// - Primary row: Kind (9 locked kinds) + Scope (all, personal, project).
// - "More filters (4)" disclosure: Source, Engine, Trust, Usage.
//   Sticky via sessionStorage ('ikenga.ngwa.morefilters'), forced open if any hidden facet active.
// - Action row: Disclose button, Clear all button, Group by pkg button, search input.

import { useState, useMemo } from 'react';
import { ChevronRight, X, Layers, Search } from 'lucide-react';
import type { NgwaItem } from '@ikenga/contract';
import { resolveTrustFacet } from '@/lib/ngwa/enrichment';

export interface NgwaFacetsState {
	kind: string; // '*' or NgwaKind
	scope: string; // 'all' | 'personal' | 'project'
	source: string; // '*' or NgwaSource
	engine: string; // '*' or 'claude' | 'codex' | 'gemini'
	trust: string; // '*' or TrustFacetValue
	usage: string; // '*' | 'week' | 'never'
	search: string;
	groupByPkg: boolean;
}

export const DEFAULT_FACETS: NgwaFacetsState = {
	kind: '*',
	scope: 'all',
	source: '*',
	engine: '*',
	trust: '*',
	usage: '*',
	search: '',
	groupByPkg: true,
};

const LOCKED_KINDS: Array<{ id: string; label: string }> = [
	{ id: '*', label: 'all' },
	{ id: 'app', label: 'app' },
	{ id: 'engine', label: 'engine' },
	{ id: 'tool', label: 'tool' },
	{ id: 'skill', label: 'skill' },
	{ id: 'agent', label: 'agent' },
	{ id: 'command', label: 'command' },
	{ id: 'hook', label: 'hook' },
	{ id: 'workflow', label: 'workflow' },
	{ id: 'schedule', label: 'schedule' },
];

const SCOPES: Array<{ id: string; label: string }> = [
	{ id: 'all', label: 'all' },
	{ id: 'personal', label: 'personal' },
	{ id: 'project', label: 'project' },
];

const SOURCES: Array<{ id: string; label: string }> = [
	{ id: '*', label: 'all' },
	{ id: 'builtin', label: 'builtin' },
	{ id: 'registry', label: 'registry' },
	{ id: 'git', label: 'git' },
	{ id: 'npx', label: 'npx' },
	{ id: 'local', label: 'local' },
	{ id: 'dev', label: 'dev' },
];

const ENGINES: Array<{ id: string; label: string }> = [
	{ id: '*', label: 'all' },
	{ id: 'claude', label: 'claude' },
	{ id: 'codex', label: 'codex' },
	{ id: 'gemini', label: 'gemini' },
];

const TRUSTS: Array<{ id: string; label: string }> = [
	{ id: '*', label: 'all' },
	{ id: 'builtin', label: 'builtin' },
	{ id: 'signed', label: 'signed' },
	{ id: 'unsigned', label: 'unsigned' },
	{ id: 'review', label: 'review' },
];

const USAGES: Array<{ id: string; label: string }> = [
	{ id: '*', label: 'all' },
	{ id: 'week', label: 'used this week' },
	{ id: 'never', label: 'never' },
];

const MF_KEY = 'ikenga.ngwa.morefilters';

export interface NgwaFacetBarProps {
	items: NgwaItem[];
	facets: NgwaFacetsState;
	onChange: (next: NgwaFacetsState) => void;
}

export function NgwaFacetBar({ items, facets, onChange }: NgwaFacetBarProps) {
	// Sticky disclosure state
	const [moreOpen, setMoreOpen] = useState(() => {
		try {
			return sessionStorage.getItem(MF_KEY) === '1';
		} catch {
			return false;
		}
	});

	const hiddenActive = useMemo(() => {
		return (
			facets.source !== '*' ||
			facets.engine !== '*' ||
			facets.trust !== '*' ||
			facets.usage !== '*'
		);
	}, [facets.source, facets.engine, facets.trust, facets.usage]);

	// P4: A hidden facet being active forces the disclosure open
	const isDisclosed = hiddenActive || moreOpen;

	function toggleMore() {
		const next = !isDisclosed;
		setMoreOpen(next);
		try {
			sessionStorage.setItem(MF_KEY, next ? '1' : '0');
		} catch {
			// ignore private browsing
		}
	}

	const hasActiveFilters = useMemo(() => {
		return (
			facets.kind !== '*' ||
			facets.scope !== 'all' ||
			facets.source !== '*' ||
			facets.engine !== '*' ||
			facets.trust !== '*' ||
			facets.usage !== '*' ||
			facets.search.trim().length > 0
		);
	}, [facets]);

	function clearAll() {
		onChange({
			...DEFAULT_FACETS,
			groupByPkg: facets.groupByPkg,
		});
	}

	// Facet count computations
	const counts = useMemo(() => {
		const matchExcept = (it: NgwaItem, exceptKey: keyof NgwaFacetsState): boolean => {
			if (exceptKey !== 'kind' && facets.kind !== '*' && it.kind !== facets.kind) return false;
			if (exceptKey !== 'scope' && facets.scope !== 'all' && it.scope.kind !== facets.scope)
				return false;
			if (exceptKey !== 'source' && facets.source !== '*' && it.origin.source !== facets.source)
				return false;
			if (
				exceptKey !== 'engine' &&
				facets.engine !== '*' &&
				!it.engines.includes(facets.engine)
			)
				return false;
			if (exceptKey !== 'trust' && facets.trust !== '*') {
				if (resolveTrustFacet(it.trust) !== facets.trust) return false;
			}
			if (exceptKey !== 'usage' && facets.usage !== '*') {
				const hasUsed = it.usage !== null && (it.usage.count_7d ?? 0) > 0;
				if (facets.usage === 'week' && !hasUsed) return false;
				if (facets.usage === 'never' && it.usage !== null && (it.usage.count_30d ?? 0) > 0)
					return false;
			}
			if (facets.search) {
				const q = facets.search.toLowerCase();
				const text = `${it.name} ${it.display_name} ${it.id} ${it.install_path ?? ''}`.toLowerCase();
				if (!text.includes(q)) return false;
			}
			return true;
		};

		const kindCounts: Record<string, number> = {};
		for (const k of LOCKED_KINDS) {
			kindCounts[k.id] = items.filter(
				(it) => (k.id === '*' || it.kind === k.id) && matchExcept(it, 'kind')
			).length;
		}

		const scopeCounts: Record<string, number> = {};
		for (const sc of SCOPES) {
			scopeCounts[sc.id] = items.filter(
				(it) => (sc.id === 'all' || it.scope.kind === sc.id) && matchExcept(it, 'scope')
			).length;
		}

		const sourceCounts: Record<string, number> = {};
		for (const src of SOURCES) {
			sourceCounts[src.id] = items.filter(
				(it) => (src.id === '*' || it.origin.source === src.id) && matchExcept(it, 'source')
			).length;
		}

		const engineCounts: Record<string, number> = {};
		for (const eng of ENGINES) {
			engineCounts[eng.id] = items.filter(
				(it) => (eng.id === '*' || it.engines.includes(eng.id)) && matchExcept(it, 'engine')
			).length;
		}

		const trustCounts: Record<string, number> = {};
		for (const tr of TRUSTS) {
			trustCounts[tr.id] = items.filter(
				(it) => (tr.id === '*' || resolveTrustFacet(it.trust) === tr.id) && matchExcept(it, 'trust')
			).length;
		}

		const usageCounts: Record<string, number> = {};
		for (const us of USAGES) {
			usageCounts[us.id] = items.filter((it) => {
				if (!matchExcept(it, 'usage')) return false;
				if (us.id === '*') return true;
				const hasUsed = it.usage !== null && (it.usage.count_7d ?? 0) > 0;
				if (us.id === 'week') return hasUsed;
				if (us.id === 'never') return it.usage === null || (it.usage.count_30d ?? 0) === 0;
				return true;
			}).length;
		}

		return {
			kind: kindCounts,
			scope: scopeCounts,
			source: sourceCounts,
			engine: engineCounts,
			trust: trustCounts,
			usage: usageCounts,
		};
	}, [items, facets]);

	return (
		<div className="facetbar" data-ifacets>
			{/* ── Primary Row: Kind + Scope ── */}
			<div className="frow2">
				<span className="flabel">Kind</span>
				{LOCKED_KINDS.map((k) => {
					const on = facets.kind === k.id;
					const cnt = counts.kind[k.id] ?? 0;
					return (
						<button
							key={k.id}
							type="button"
							className={`chip ${on ? 'on' : ''}`}
							aria-pressed={on}
							disabled={cnt === 0 && !on}
							onClick={() => onChange({ ...facets, kind: on && k.id !== '*' ? '*' : k.id })}
						>
							{k.label} <span className="n">{cnt}</span>
						</button>
					);
				})}
				<span className="toolsep" />
				<span className="flabel" style={{ width: 'auto' }}>Scope</span>
				{SCOPES.map((sc) => {
					const on = facets.scope === sc.id;
					const cnt = counts.scope[sc.id] ?? 0;
					return (
						<button
							key={sc.id}
							type="button"
							className={`chip ${on ? 'on' : ''}`}
							aria-pressed={on}
							disabled={cnt === 0 && !on}
							onClick={() => onChange({ ...facets, scope: on && sc.id !== 'all' ? 'all' : sc.id })}
						>
							{sc.label} <span className="n">{cnt}</span>
						</button>
					);
				})}
			</div>

			{/* ── More Filters Disclosure (Source, Engine, Trust, Usage) ── */}
			{isDisclosed && (
				<div className="morefilters" id="ngwa-morefilters">
					<div className="frow2">
						<span className="flabel">Source</span>
						{SOURCES.map((src) => {
							const on = facets.source === src.id;
							const cnt = counts.source[src.id] ?? 0;
							return (
								<button
									key={src.id}
									type="button"
									className={`chip ${on ? 'on' : ''}`}
									aria-pressed={on}
									disabled={cnt === 0 && !on}
									onClick={() => onChange({ ...facets, source: on && src.id !== '*' ? '*' : src.id })}
								>
									{src.label} <span className="n">{cnt}</span>
								</button>
							);
						})}
						<span className="toolsep" />
						<span className="flabel" style={{ width: 'auto' }}>Engine</span>
						{ENGINES.map((eng) => {
							const on = facets.engine === eng.id;
							const cnt = counts.engine[eng.id] ?? 0;
							return (
								<button
									key={eng.id}
									type="button"
									className={`chip ${on ? 'on' : ''}`}
									aria-pressed={on}
									disabled={cnt === 0 && !on}
									onClick={() => onChange({ ...facets, engine: on && eng.id !== '*' ? '*' : eng.id })}
								>
									{eng.label} <span className="n">{cnt}</span>
								</button>
							);
						})}
					</div>

					<div className="frow2">
						<span className="flabel">Trust</span>
						{TRUSTS.map((tr) => {
							const on = facets.trust === tr.id;
							const cnt = counts.trust[tr.id] ?? 0;
							return (
								<button
									key={tr.id}
									type="button"
									className={`chip ${on ? 'on' : ''}`}
									aria-pressed={on}
									disabled={cnt === 0 && !on}
									onClick={() => onChange({ ...facets, trust: on && tr.id !== '*' ? '*' : tr.id })}
								>
									{tr.label} <span className="n">{cnt}</span>
								</button>
							);
						})}
						<span className="toolsep" />
						<span className="flabel" style={{ width: 'auto' }}>Usage</span>
						{USAGES.map((us) => {
							const on = facets.usage === us.id;
							const cnt = counts.usage[us.id] ?? 0;
							return (
								<button
									key={us.id}
									type="button"
									className={`chip ${on ? 'on' : ''}`}
									aria-pressed={on}
									disabled={cnt === 0 && !on}
									onClick={() => onChange({ ...facets, usage: on && us.id !== '*' ? '*' : us.id })}
								>
									{us.label} <span className="n">{cnt}</span>
								</button>
							);
						})}
					</div>
				</div>
			)}

			{/* ── Action Row: Disclosure, Clear, Group, Search ── */}
			<div className="frow2">
				<button
					type="button"
					className="chip disclose"
					aria-expanded={isDisclosed}
					aria-controls="ngwa-morefilters"
					onClick={toggleMore}
					title={hiddenActive ? 'A hidden facet is active, so this stays open' : ''}
				>
					<ChevronRight className="h-3 w-3 caret" />
					{isDisclosed ? 'Fewer filters' : 'More filters (4)'}
				</button>

				{hasActiveFilters && (
					<button type="button" className="chip clear" onClick={clearAll}>
						<X className="h-3 w-3" /> Clear all
					</button>
				)}

				<button
					type="button"
					className={`chip ${facets.groupByPkg ? 'on' : ''}`}
					aria-pressed={facets.groupByPkg}
					onClick={() => onChange({ ...facets, groupByPkg: !facets.groupByPkg })}
				>
					<Layers className="h-3 w-3" /> Group by pkg
				</button>

				<div className="search">
					<Search className="h-3.5 w-3.5" />
					<input
						type="text"
						placeholder="Filter by name or path…"
						aria-label="Filter installed equipment"
						value={facets.search}
						onChange={(e) => onChange({ ...facets, search: e.target.value })}
					/>
					{facets.search && (
						<button
							type="button"
							className="text-muted-foreground hover:text-foreground"
							onClick={() => onChange({ ...facets, search: '' })}
							aria-label="Clear search"
						>
							<X className="h-3 w-3" />
						</button>
					)}
				</div>
			</div>
		</div>
	);
}
