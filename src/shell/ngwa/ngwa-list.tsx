// Ngwa Equipment List Surface (WP-15 / locked D-02).
//
// Unifies all installed equipment into one faceted list:
// - Kind & Scope primary facets + "More filters" (Source, Engine, Trust, Usage)
// - Group by pkg with closure child rows
// - Invariant: usage === null renders as "—", measured 0 renders as "0 sessions"
// - Unreadable source banner if any subsystem failed (Gate §2)
// - Synchronized detail pane

import { useState, useMemo } from 'react';
import {
	AlertTriangle,
	AppWindow,
	Bot,
	Clock,
	Layers,
	RefreshCw,
	Shield,
	Slash,
	Terminal,
	User,
	Zap,
} from 'lucide-react';
import type { NgwaItem, NgwaKind } from '@ikenga/contract';
import { NgwaFacetBar, DEFAULT_FACETS, type NgwaFacetsState } from './ngwa-facet-bar';
import { NgwaDetailPane } from './ngwa-detail-pane';
import {
	formatUsageDisplay,
	formatUsageTooltip,
	resolveTrustFacet,
} from '@/lib/ngwa/enrichment';
import './ngwa.css';

export interface NgwaListProps {
	items: NgwaItem[];
	unreadableSources?: Array<{ source: string; error: string | null }>;
	isLoading?: boolean;
	error?: Error | null;
	onToggleState?: (item: NgwaItem) => void;
	onUpdate?: (item: NgwaItem) => void;
	onHandToChi?: (item: NgwaItem) => void;
}

export function kindIcon(kind: NgwaKind) {
	switch (kind) {
		case 'app':
			return <AppWindow className="h-3.5 w-3.5 flex-none" />;
		case 'engine':
			return <Bot className="h-3.5 w-3.5 flex-none" />;
		case 'tool':
			return <Terminal className="h-3.5 w-3.5 flex-none" />;
		case 'skill':
			return <Zap className="h-3.5 w-3.5 flex-none" />;
		case 'agent':
			return <User className="h-3.5 w-3.5 flex-none" />;
		case 'command':
			return <Slash className="h-3.5 w-3.5 flex-none" />;
		case 'hook':
			return <Shield className="h-3.5 w-3.5 flex-none" />;
		case 'workflow':
			return <RefreshCw className="h-3.5 w-3.5 flex-none" />;
		case 'schedule':
			return <Clock className="h-3.5 w-3.5 flex-none" />;
		default:
			return <Layers className="h-3.5 w-3.5 flex-none" />;
	}
}

export function NgwaList({
	items,
	unreadableSources = [],
	isLoading = false,
	error = null,
	onToggleState,
	onUpdate,
	onHandToChi,
}: NgwaListProps) {
	const [facets, setFacets] = useState<NgwaFacetsState>(DEFAULT_FACETS);
	const [selectedId, setSelectedId] = useState<string | null>(null);

	// Filter items according to active facets
	const filteredItems = useMemo(() => {
		return items.filter((it) => {
			if (facets.kind !== '*' && it.kind !== facets.kind) return false;
			if (facets.scope !== 'all' && it.scope.kind !== facets.scope) return false;
			if (facets.source !== '*' && it.origin.source !== facets.source) return false;
			if (facets.engine !== '*' && !it.engines.includes(facets.engine)) return false;
			if (facets.trust !== '*' && resolveTrustFacet(it.trust) !== facets.trust) return false;
			if (facets.usage !== '*') {
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
		});
	}, [items, facets]);

	// Auto-select first item if current selection is invalid or null
	const activeItem = useMemo(() => {
		if (!filteredItems.length) return null;
		if (!selectedId) return filteredItems[0];
		return filteredItems.find((i) => i.id === selectedId) ?? filteredItems[0];
	}, [filteredItems, selectedId]);

	// Parent map for grouping
	const { topLevelItems, childMap } = useMemo(() => {
		if (!facets.groupByPkg) {
			return { topLevelItems: filteredItems, childMap: new Map<string, NgwaItem[]>() };
		}
		const children = new Map<string, NgwaItem[]>();
		const top: NgwaItem[] = [];

		for (const it of filteredItems) {
			if (it.owner_pkg_id && it.owner_pkg_id !== it.id) {
				const existing = children.get(it.owner_pkg_id) ?? [];
				existing.push(it);
				children.set(it.owner_pkg_id, existing);
			} else {
				top.push(it);
			}
		}

		// Also surface child items whose parent is filtered out or missing as promoted rows
		const allParents = new Set(top.map((t) => t.id));
		for (const [parentId, childList] of children.entries()) {
			if (!allParents.has(parentId)) {
				for (const orphanChild of childList) {
					top.push(orphanChild);
				}
				children.delete(parentId);
			}
		}

		return { topLevelItems: top, childMap: children };
	}, [filteredItems, facets.groupByPkg]);

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			{/* ── Unreadable Source Banners (Gate §2) ── */}
			{unreadableSources.map((s) => (
				<div key={s.source} className="source-banner" role="alert">
					<AlertTriangle className="h-4 w-4 flex-none" />
					<span>
						<strong>{s.source} unreadable</strong>
						{s.error ? `: ${s.error}` : ' — could not scan subsystem'}
					</span>
				</div>
			))}

			{/* ── Facet Bar ── */}
			<NgwaFacetBar items={items} facets={facets} onChange={setFacets} />

			{/* ── Split List & Detail ── */}
			<div className="split">
				<div className="listcol">
					<div className="lhead">
						<span>Equipment</span>
						<span className="c-usage">Sessions</span>
						<span className="c-state">Status</span>
					</div>

					<div className="sc flex-1 min-h-0" role="listbox" aria-label="Installed equipment">
						{isLoading && (
							<div className="empty">
								<span className="emberbar">
									<i />
									Scanning equipment catalogue (cold scan may take a moment)...
								</span>
							</div>
						)}

						{error && (
							<div className="empty text-destructive">
								Failed to load equipment: {String(error)}
							</div>
						)}

						{!isLoading && !error && filteredItems.length === 0 && (
							<div className="empty" data-iempty>
								Nothing matches these facets. Usage is only known for equipment the transcript scanner has recorded — items with no record read “—”.
							</div>
						)}

						{!isLoading &&
							topLevelItems.map((item) => {
								const children = childMap.get(item.id) ?? [];
								return (
									<div key={item.id}>
										<ItemRow
											item={item}
											isSelected={activeItem?.id === item.id}
											onSelect={() => setSelectedId(item.id)}
										/>
										{children.map((child) => (
											<ItemRow
												key={child.id}
												item={child}
												isChild
												parentName={item.name}
												isSelected={activeItem?.id === child.id}
												onSelect={() => setSelectedId(child.id)}
											/>
										))}
									</div>
								);
							})}
					</div>
				</div>

				{/* ── Detail Pane ── */}
				{activeItem ? (
					<NgwaDetailPane
						item={activeItem}
						onToggleState={onToggleState}
						onUpdate={onUpdate}
						onHandToChi={onHandToChi}
					/>
				) : (
					<aside className="detailcol">
						<div className="empty">No item selected.</div>
					</aside>
				)}
			</div>
		</div>
	);
}

function ItemRow({
	item,
	isChild = false,
	parentName,
	isSelected,
	onSelect,
}: {
	item: NgwaItem;
	isChild?: boolean;
	parentName?: string;
	isSelected: boolean;
	onSelect: () => void;
}) {
	const usageText = formatUsageDisplay(item.usage);
	const usageTooltip = formatUsageTooltip(item.usage);

	return (
		<button
			type="button"
			role="option"
			aria-selected={isSelected}
			data-id={item.id}
			data-kind={item.kind}
			className={`irow ${isChild ? 'child' : ''} ${isSelected ? 'sel' : ''} ${
				item.state === 'disabled' ? 'off' : ''
			}`}
			onClick={onSelect}
		>
			{kindIcon(item.kind)}
			<span className="nm">{item.display_name || item.name}</span>
			<span className={`kind k-${item.kind}`}>{item.kind}</span>
			<span className="meta">{item.scope.kind}</span>
			<span className="meta mono">{item.origin.source}</span>

			{/* Engine Placement Dots */}
			<span
				className="dots"
				title={`placed in: ${item.engines.length ? item.engines.join(', ') : 'none'}`}
			>
				{['claude', 'codex', 'gemini'].map((eng) => (
					<i key={eng} className={item.engines.includes(eng) ? 'on' : ''} />
				))}
			</span>

			{isChild && parentName && (
				<span className="grpchip" title={`contributed by ${parentName}`}>
					{parentName}
				</span>
			)}

			<span className="tail">
				<span className="c-usage" title={usageTooltip} data-usage-value={usageText}>
					{usageText}
				</span>
				<span className="c-state">
					<span className={`state s-${item.state}`}>{item.state}</span>
				</span>
			</span>
		</button>
	);
}
