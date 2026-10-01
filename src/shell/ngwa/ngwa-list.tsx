// Ngwa Equipment List Surface (WP-15 / locked D-02).
//
// Unifies all installed equipment into one faceted list:
// - Kind & Scope primary facets + "More filters" (Source, Engine, Trust, Usage)
// - Group by pkg with closure child rows
// - Invariant: usage === null renders as "—", measured 0 renders as "0 sessions"
// - Unreadable source banner if any subsystem failed (Gate §2)
// - Synchronized detail pane

import { useCallback, useState, useMemo } from 'react';
import { AlertTriangle } from 'lucide-react';
import type { NgwaItem } from '@ikenga/contract';
import { NgwaFacetBar, DEFAULT_FACETS, type NgwaFacetsState } from './ngwa-facet-bar';
import { NgwaDetailPane } from './ngwa-detail-pane';
import { NgwaPopMenu, type PopItem } from './ngwa-scope-ops';
import type { NgwaAct, NgwaActionStatus, NgwaItemActionSet } from '@/lib/ngwa/use-ngwa-actions';
import { formatUsageDisplay, formatUsageTooltip, resolveTrustFacet } from '@/lib/ngwa/enrichment';
import { kindIcon } from '@/lib/ngwa/kind-icon';
import './ngwa.css';

export { kindIcon };

export interface NgwaListProps {
	items: NgwaItem[];
	unreadableSources?: Array<{ source: string; error: string | null }>;
	isLoading?: boolean;
	error?: Error | null;
	/** D-02 actions for an item (detail action row + row context menu),
	 *  built by `useNgwaItemActions`. Omitted => read-only list. */
	actionsFor?: (item: NgwaItem) => NgwaItemActionSet;
	/** Double-click a row: open the full-pane item detail (D-08). */
	onOpenItem?: (item: NgwaItem) => void;
	/** Result line of the last action. */
	status?: NgwaActionStatus | null;
	/** Pre-filled name filter (the Store's "Open in Installed", R57). */
	initialSearch?: string;
}

/** D-02 row context menu (`rowMenu`): the name as group header, Disable /
 *  Enable (Space), Move to project / personal, Copy to…, Update, Remove…,
 *  Open folder (↵), Hand to Chi. */
export function rowMenuItems(
	item: NgwaItem,
	a: NgwaItemActionSet,
	openCopy: () => void
): PopItem[] {
	const act = (x: NgwaAct, extra: Partial<PopItem> = {}): PopItem => ({
		label: x.label,
		disabledReason: x.disabledReason,
		onSelect: x.run,
		...extra,
	});
	return [
		{ group: true, label: item.display_name || item.name },
		act(a.toggle, { k: 'Space' }),
		{ sep: true, label: '' },
		act(a.moveToProject),
		act(a.moveToPersonal),
		{
			label: 'Copy to…',
			disabledReason: a.copy.disabledReason,
			keepOpen: true,
			onSelect: openCopy,
		},
		{ sep: true, label: '' },
		act(a.update),
		act(a.remove, { danger: true }),
		{ sep: true, label: '' },
		act(a.openFolder, { k: '↵' }),
		act(a.handToChi),
	];
}

interface RowMenu {
	id: string;
	x: number;
	y: number;
	mode: 'menu' | 'copy';
}

export function NgwaList({
	items,
	unreadableSources = [],
	isLoading = false,
	error = null,
	actionsFor,
	onOpenItem,
	status = null,
	initialSearch,
}: NgwaListProps) {
	const [facets, setFacets] = useState<NgwaFacetsState>(() =>
		initialSearch ? { ...DEFAULT_FACETS, search: initialSearch } : DEFAULT_FACETS
	);
	const [selectedId, setSelectedId] = useState<string | null>(null);
	const [menu, setMenu] = useState<RowMenu | null>(null);
	const closeMenu = useCallback(() => setMenu(null), []);

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

	const menuItem = menu ? (items.find((i) => i.id === menu.id) ?? null) : null;

	/** Context menu, Space (toggle), ↵ (open folder), double-click (open the
	 *  item detail) — the D-02 row bindings. */
	function rowHandlers(item: NgwaItem) {
		return {
			onContextMenu: actionsFor
				? (e: React.MouseEvent) => {
						e.preventDefault();
						setSelectedId(item.id);
						setMenu({ id: item.id, x: e.clientX, y: e.clientY, mode: 'menu' });
					}
				: undefined,
			onKeyDown: actionsFor
				? (e: React.KeyboardEvent) => {
						if (e.key !== ' ' && e.key !== 'Enter') return;
						e.preventDefault();
						setSelectedId(item.id);
						const a = actionsFor(item);
						const act = e.key === ' ' ? a.toggle : a.openFolder;
						if (act.disabledReason === undefined) act.run();
					}
				: undefined,
			onDoubleClick: onOpenItem ? () => onOpenItem(item) : undefined,
		};
	}

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
											{...rowHandlers(item)}
										/>
										{children.map((child) => (
											<ItemRow
												key={child.id}
												item={child}
												isChild
												parentName={item.name}
												isSelected={activeItem?.id === child.id}
												onSelect={() => setSelectedId(child.id)}
												{...rowHandlers(child)}
											/>
										))}
									</div>
								);
							})}
					</div>
					{status && (
						<div className={`mstatus ${status.tone}`} role="status" data-mstatus>
							{status.text}
						</div>
					)}
				</div>

				{/* ── Detail Pane ── */}
				{activeItem ? (
					<NgwaDetailPane item={activeItem} actions={actionsFor?.(activeItem)} />
				) : (
					<aside className="detailcol">
						<div className="empty">No item selected.</div>
					</aside>
				)}
			</div>

			{/* ── Row context menu (D-02 rowMenu) ── */}
			{menu && menuItem && actionsFor && (
				<NgwaPopMenu
					pop={
						menu.mode === 'copy'
							? { id: `copy:${menu.id}`, title: 'Copy to', items: actionsFor(menuItem).copy.targets }
							: {
									id: menu.id,
									title: menuItem.display_name || menuItem.name,
									items: rowMenuItems(menuItem, actionsFor(menuItem), () =>
										setMenu((m) => (m ? { ...m, mode: 'copy' } : m))
									),
								}
					}
					onClose={closeMenu}
					className="ctxpop"
					style={{ left: menu.x, top: menu.y }}
					autoFocus={false}
				/>
			)}
		</div>
	);
}

function ItemRow({
	item,
	isChild = false,
	parentName,
	isSelected,
	onSelect,
	onContextMenu,
	onKeyDown,
	onDoubleClick,
}: {
	item: NgwaItem;
	isChild?: boolean;
	parentName?: string;
	isSelected: boolean;
	onSelect: () => void;
	onContextMenu?: (e: React.MouseEvent) => void;
	onKeyDown?: (e: React.KeyboardEvent) => void;
	onDoubleClick?: () => void;
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
			onContextMenu={onContextMenu}
			onKeyDown={onKeyDown}
			onDoubleClick={onDoubleClick}
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
