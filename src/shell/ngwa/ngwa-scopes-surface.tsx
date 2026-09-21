// Ngwa Scopes Matrix Surface (WP-16 / locked D-02 frame-workbench-v4.html).
//
// Surfaces the 3-axis matrix: Equipment × Scopes × Engines.
// - Scope columns: Personal (~/.claude) + Active Project (.claude).
// - Engine columns: claude, codex (gemini column hidden if uninstalled per P16).
// - Precedence conflict detection in sidenote drawer.
// - Cell actions: Enable, Disable, Promote, Move, Remove.

import { useState, useMemo } from 'react';
import {
	AlertTriangle,
	ArrowRight,
	Bot,
	Circle,
	CircleDot,
	Minus,
} from 'lucide-react';
import type { NgwaItem } from '@ikenga/contract';
import { kindIcon } from './ngwa-list';
import './ngwa.css';

export interface NgwaScopesSurfaceProps {
	items: NgwaItem[];
	isLoading?: boolean;
	error?: Error | null;
	activeProjectName?: string;
	onEnableItem?: (item: NgwaItem, scope: 'personal' | 'project') => void;
	onDisableItem?: (item: NgwaItem, scope: 'personal' | 'project') => void;
	onPromoteItem?: (item: NgwaItem) => void;
	onRemoveItem?: (item: NgwaItem, scope: 'personal' | 'project') => void;
	onEnableAll?: (target: string) => void;
	onInstallEngine?: (engine: string) => void;
}

const SCOPE_KINDS: readonly { id: string; label: string }[] = [
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

export interface ScopeConflict {
	name: string;
	personalItem: NgwaItem;
	projectItem: NgwaItem;
}

export function NgwaScopesSurface({
	items,
	isLoading = false,
	error = null,
	activeProjectName = 'project',
	onEnableItem,
	onDisableItem,
	onPromoteItem,
	onRemoveItem,
	onEnableAll,
	onInstallEngine,
}: NgwaScopesSurfaceProps) {
	const [selectedKind, setSelectedKind] = useState<string>('*');
	const [activeCellId, setActiveCellId] = useState<string | null>(null);

	// Detect installed engine adapters
	const installedEngines = useMemo(() => {
		const engines = new Set<string>();
		for (const it of items) {
			for (const eng of it.engines) {
				engines.add(eng);
			}
			if (it.kind === 'engine') {
				engines.add(it.name);
			}
		}
		return engines;
	}, [items]);

	const hasGemini = installedEngines.has('gemini');

	// Group items by canonical name to detect cross-scope versions
	const { equipmentByName, conflicts } = useMemo(() => {
		const byName = new Map<string, { personal?: NgwaItem; project?: NgwaItem; all: NgwaItem[] }>();
		const conflictList: ScopeConflict[] = [];

		for (const it of items) {
			const existing = byName.get(it.name) ?? { all: [] };
			existing.all.push(it);
			if (it.scope.kind === 'personal') {
				existing.personal = it;
			} else if (it.scope.kind === 'project') {
				existing.project = it;
			}
			byName.set(it.name, existing);
		}

		for (const [name, entry] of byName.entries()) {
			if (entry.personal && entry.project) {
				if (entry.personal.version !== entry.project.version) {
					conflictList.push({
						name,
						personalItem: entry.personal,
						projectItem: entry.project,
					});
				}
			}
		}

		return { equipmentByName: byName, conflicts: conflictList };
	}, [items]);

	// Filter equipment list
	const filteredItems = useMemo(() => {
		// Unique by name for the equipment rows
		const unique: NgwaItem[] = [];
		const seen = new Set<string>();

		for (const it of items) {
			if (!seen.has(it.name)) {
				seen.add(it.name);
				if (selectedKind === '*' || it.kind === selectedKind) {
					unique.push(it);
				}
			}
		}

		return unique.sort((a, b) => a.name.localeCompare(b.name));
	}, [items, selectedKind]);

	const scopeCount = 2; // Personal + Project
	const engineCount = hasGemini ? 3 : 2;

	if (isLoading) {
		return (
			<div className="view-ngwa flex-1 min-h-0 flex items-center justify-center">
				<span className="text-sm text-muted-foreground">Loading scopes matrix…</span>
			</div>
		);
	}

	if (error) {
		return (
			<div className="view-ngwa flex-1 min-h-0 p-4">
				<div className="source-banner">
					<AlertTriangle className="h-4 w-4" />
					<span>Failed to load scopes: {error.message}</span>
				</div>
			</div>
		);
	}

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			{/* ── Facet Bar ── */}
			<div className="facetbar">
				<div className="frow2">
					<span className="flabel">Kind</span>
					{SCOPE_KINDS.map((k) => {
						const cnt =
							k.id === '*'
								? filteredItems.length
								: items.filter((it) => it.kind === k.id).length;
						const on = selectedKind === k.id;
						return (
							<button
								key={k.id}
								type="button"
								data-mk={k.id}
								className={`chip ${on ? 'on' : ''}`}
								aria-pressed={on}
								onClick={() => setSelectedKind(k.id)}
							>
								{k.label} <span className="n">{cnt}</span>
							</button>
						);
					})}
					<span className="meta ml-auto" data-mcount>
						{filteredItems.length} items × {scopeCount} scopes × {engineCount} engines
					</span>
				</div>
			</div>

			{/* ── Matrix Body & Sidenote ── */}
			<div className="mwrap">
				<div className="mcol">
					<div className="mscroll sc">
						<table className="matrix" aria-label="Scope and Engine Matrix">
							<thead>
								<tr>
									<th className="left">Equipment</th>
									<th>
										<span className="colname">Personal</span>
										<span className="colsub">~/.claude</span>
										<button
											type="button"
											className="chip on text-xs mt-1"
											style={{ height: '20px', padding: '0 6px' }}
											onClick={() => onEnableAll?.('personal')}
										>
											Enable all
										</button>
									</th>
									<th>
										<span className="colname">{activeProjectName}</span>
										<span className="colsub">.claude</span>
										<button
											type="button"
											className="chip on text-xs mt-1"
											style={{ height: '20px', padding: '0 6px' }}
											onClick={() => onEnableAll?.('project')}
										>
											Enable all
										</button>
									</th>
									<th className="eng">
										<span className="colname">claude</span>
										<span className="colsub">2.0</span>
										<button
											type="button"
											className="chip on text-xs mt-1"
											style={{ height: '20px', padding: '0 6px' }}
											onClick={() => onEnableAll?.('claude')}
										>
											Enable all
										</button>
									</th>
									<th className="eng">
										<span className="colname">codex</span>
										<span className="colsub">ok</span>
										<button
											type="button"
											className="chip on text-xs mt-1"
											style={{ height: '20px', padding: '0 6px' }}
											onClick={() => onEnableAll?.('codex')}
										>
											Enable all
										</button>
									</th>
									{hasGemini && (
										<th className="eng">
											<span className="colname">gemini</span>
											<span className="colsub">ok</span>
											<button
												type="button"
												className="chip on text-xs mt-1"
												style={{ height: '20px', padding: '0 6px' }}
												onClick={() => onEnableAll?.('gemini')}
											>
												Enable all
											</button>
										</th>
									)}
								</tr>
							</thead>
							<tbody data-mbody>
								{filteredItems.map((item) => {
									const entry = equipmentByName.get(item.name);
									const pItem = entry?.personal;
									const projItem = entry?.project;
									const isConflict = Boolean(pItem && projItem && pItem.version !== projItem.version);

									return (
										<tr key={item.id}>
											<td className="item">
												<div className="in">
													{kindIcon(item.kind)}
													<span className="font-medium text-xs">
														{item.display_name || item.name}
													</span>
													<span className={`kind k-${item.kind}`}>{item.kind}</span>
												</div>
											</td>

											{/* Personal Scope Cell */}
											<td className="cell">
												<ScopeCell
													item={pItem ?? null}
													baseItem={item}
													scope="personal"
													isConflict={isConflict}
													isOpen={activeCellId === `${item.id}:personal`}
													onToggleMenu={() =>
														setActiveCellId(
															activeCellId === `${item.id}:personal`
																? null
																: `${item.id}:personal`
														)
													}
													onEnable={() => {
														setActiveCellId(null);
														if (pItem) onEnableItem?.(pItem, 'personal');
													}}
													onDisable={() => {
														setActiveCellId(null);
														if (pItem) onDisableItem?.(pItem, 'personal');
													}}
													onPromote={() => {
														setActiveCellId(null);
														if (pItem) onPromoteItem?.(pItem);
													}}
													onRemove={() => {
														setActiveCellId(null);
														if (pItem) onRemoveItem?.(pItem, 'personal');
													}}
												/>
											</td>

											{/* Project Scope Cell */}
											<td className="cell">
												<ScopeCell
													item={projItem ?? null}
													baseItem={item}
													scope="project"
													isConflict={isConflict}
													isOpen={activeCellId === `${item.id}:project`}
													onToggleMenu={() =>
														setActiveCellId(
															activeCellId === `${item.id}:project`
																? null
																: `${item.id}:project`
														)
													}
													onEnable={() => {
														setActiveCellId(null);
														if (projItem) onEnableItem?.(projItem, 'project');
													}}
													onDisable={() => {
														setActiveCellId(null);
														if (projItem) onDisableItem?.(projItem, 'project');
													}}
													onPromote={() => {
														setActiveCellId(null);
														if (projItem) onPromoteItem?.(projItem);
													}}
													onRemove={() => {
														setActiveCellId(null);
														if (projItem) onRemoveItem?.(projItem, 'project');
													}}
												/>
											</td>

											{/* Claude Engine Placement */}
											<td className="cell eng">
												<EngineCell
													placed={item.engines.includes('claude')}
													engine="claude"
												/>
											</td>

											{/* Codex Engine Placement */}
											<td className="cell eng">
												<EngineCell
													placed={item.engines.includes('codex')}
													engine="codex"
												/>
											</td>

											{/* Gemini Engine Placement (if present) */}
											{hasGemini && (
												<td className="cell eng">
													<EngineCell
														placed={item.engines.includes('gemini')}
														engine="gemini"
													/>
												</td>
											)}
										</tr>
									);
								})}
							</tbody>
						</table>
					</div>

					{/* Footnotice: P16 uninstalled Gemini engine adapter */}
					{!hasGemini && (
						<div className="mfoot" data-geminifoot>
							<Bot className="h-3.5 w-3.5 text-muted-foreground" />
							<span>
								<strong>gemini</strong> is not installed, so its column is not shown.
							</span>
							<button
								type="button"
								className="chip ml-auto"
								data-act="installengine"
								data-gemini
								onClick={() => onInstallEngine?.('gemini')}
							>
								Install engine
							</button>
						</div>
					)}
				</div>

				{/* ── Sidenote Drawer ── */}
				<aside className="sidenote sc" data-sidenote aria-label="Matrix Precedence and Legend">
					<div className="subhead first font-semibold text-xs text-muted-foreground uppercase tracking-wider mb-2">
						Precedence
					</div>

					{conflicts.length > 0 ? (
						conflicts.map((c) => (
							<div
								key={c.name}
								className="hrow mb-3 rounded border border-destructive/40 bg-muted/40 p-2"
							>
								<AlertTriangle className="h-4 w-4 flex-none text-destructive mt-0.5" />
								<div className="txt">
									<span className="t1 font-medium text-xs text-foreground">
										{c.name} exists twice
									</span>
									<span className="t2 text-xs text-muted-foreground block mt-1">
										personal <code className="font-mono text-xs">{c.personalItem.version}</code> ·{' '}
										{activeProjectName}{' '}
										<code className="font-mono text-xs">{c.projectItem.version}</code>. The
										nearer scope wins, so inside this project every session loads{' '}
										{c.projectItem.version} and the personal copy is shadowed.
									</span>
									<div className="acts mt-2 flex gap-1">
										<button
											type="button"
											className="chip on text-xs"
											data-act="update-personal"
											onClick={() => onPromoteItem?.(c.projectItem)}
										>
											Promote to personal
										</button>
										<button
											type="button"
											className="chip clear text-xs"
											data-act="remove-personal"
											onClick={() => onRemoveItem?.(c.personalItem, 'personal')}
										>
											Remove personal
										</button>
									</div>
								</div>
							</div>
						))
					) : (
						<div className="text-xs text-muted-foreground mb-4 p-2 bg-muted/20 rounded">
							No precedence conflicts detected across scopes.
						</div>
					)}

					<div className="subhead font-semibold text-xs text-muted-foreground uppercase tracking-wider mb-2">
						Reading a cell
					</div>
					<div className="legend">
						<div>
							<CircleDot className="h-3.5 w-3.5 text-primary filled" />
							<span>enabled in this scope</span>
						</div>
						<div>
							<Circle className="h-3.5 w-3.5 text-muted-foreground" />
							<span>installed but disabled</span>
						</div>
						<div>
							<ArrowRight className="h-3.5 w-3.5 text-info" />
							<span>symlinked from the store, not copied</span>
						</div>
						<div>
							<Minus className="h-3.5 w-3.5 text-muted-foreground" />
							<span>not present here</span>
						</div>
						<div>
							<AlertTriangle className="h-3.5 w-3.5 text-destructive" />
							<span>same name at two scopes, different versions</span>
						</div>
					</div>

					<p className="note text-xs text-muted-foreground mt-4 leading-relaxed">
						Click any cell to enable, move or copy the item into that scope or engine.
						Column headers act on the whole column.
					</p>
				</aside>
			</div>
		</div>
	);
}

function ScopeCell({
	item,
	baseItem,
	scope,
	isConflict,
	isOpen,
	onToggleMenu,
	onEnable,
	onDisable,
	onPromote,
	onRemove,
}: {
	item: NgwaItem | null;
	baseItem: NgwaItem;
	scope: 'personal' | 'project';
	isConflict: boolean;
	isOpen: boolean;
	onToggleMenu: () => void;
	onEnable: () => void;
	onDisable: () => void;
	onPromote: () => void;
	onRemove: () => void;
}) {
	const present = item !== null;
	const isEnabled = item?.state === 'enabled';

	return (
		<div className="relative inline-block w-full">
			<button
				type="button"
				className={`cellbtn ${isConflict ? 'conflict' : isEnabled ? 'on' : ''} ${
					isOpen ? 'open' : ''
				}`}
				onClick={onToggleMenu}
				aria-label={`${baseItem.name} in ${scope} scope: ${
					isConflict ? 'conflict' : present ? (isEnabled ? 'enabled' : 'disabled') : 'not present'
				}`}
			>
				{isConflict ? (
					<AlertTriangle className="h-3.5 w-3.5 text-destructive" />
				) : present ? (
					isEnabled ? (
						<CircleDot className="h-3.5 w-3.5 text-primary filled" />
					) : (
						<Circle className="h-3.5 w-3.5 text-muted-foreground" />
					)
				) : (
					<Minus className="h-3 w-3 opacity-30" />
				)}
			</button>

			{/* Cell Actions Popover */}
			{isOpen && (
				<div
					className="menu"
					style={{
						position: 'absolute',
						top: '100%',
						left: '50%',
						transform: 'translateX(-50%)',
						zIndex: 100,
						minWidth: '160px',
						marginTop: '2px',
					}}
				>
					<div className="mgroup">
						{baseItem.name} · {scope}
					</div>
					{present ? (
						<>
							{isEnabled ? (
								<button type="button" className="mitem" onClick={onDisable}>
									Disable in {scope}
								</button>
							) : (
								<button type="button" className="mitem" onClick={onEnable}>
									Enable in {scope}
								</button>
							)}
							{scope === 'personal' ? (
								<button type="button" className="mitem" onClick={onPromote}>
									Promote to project
								</button>
							) : (
								<button type="button" className="mitem" onClick={onPromote}>
									Copy to personal
								</button>
							)}
							<button type="button" className="mitem text-destructive" onClick={onRemove}>
								Remove from {scope}
							</button>
						</>
					) : (
						<button type="button" className="mitem" onClick={onEnable}>
							Add to {scope}
						</button>
					)}
				</div>
			)}
		</div>
	);
}

function EngineCell({ placed, engine }: { placed: boolean; engine: string }) {
	return (
		<div className="cellbtn" title={`${engine}: ${placed ? 'placed' : 'not placed'}`}>
			{placed ? (
				<CircleDot className="h-3.5 w-3.5 text-primary filled" />
			) : (
				<Minus className="h-3 w-3 opacity-30" />
			)}
		</div>
	);
}
