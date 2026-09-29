// Ngwa Scopes Matrix Surface (WP-16 / WP-16a / locked D-02 frame-workbench-v4.html).
//
// Rows are equipment keyed by (kind, name). Columns are Personal, then one
// column per registered project scope (active project first), then one column
// per *installed* engine. Every cell value is read from the snapshot; every
// control either calls a real command through the route's `actions` or is
// disabled with a stated reason (interaction spec §1.2). Destructive actions
// sit behind a confirm that names the exact path (DEC-30); a precedence
// conflict is read from `placements[].overridden_by` (DEC-31).

import { useCallback, useMemo, useRef, useState } from 'react';
import { AlertTriangle, ArrowRight, Bot, Circle, CircleDot, HelpCircle, Minus } from 'lucide-react';
import type { NgwaItem } from '@ikenga/contract';
import type { EngineId } from '@/lib/tauri-cmd';
import { kindIcon } from './ngwa-list';
import {
	ENGINE_IDS,
	NgwaConfirmDialog,
	engineMark,
	errText,
	installedEngines,
	scopeKeyOf,
	scopeMark,
	wireOf,
	type CellMark,
	type ConfirmRequest,
	type MatrixRow,
	type NgwaScopeActions,
	type ScopeColumn,
	type ScopeConflict,
} from './ngwa-scope-model';
import {
	BUILTIN_REMOVE_REASON,
	BUSY_REASON,
	PKG_ONE_SCOPE,
	PRIMITIVE_SOURCES,
	NgwaPopMenu,
	useScopeOps,
	type OpenPop,
	type PopItem,
} from './ngwa-scope-ops';
import './ngwa.css';

export * from './ngwa-scope-model';

// ─── Surface ─────────────────────────────────────────────────────────────────


export interface NgwaScopesSurfaceProps {
	items: NgwaItem[];
	isLoading?: boolean;
	error?: Error | null;
	unreadableSources?: Array<{ source: string; error: string | null }>;
	scopes: ScopeColumn[];
	/** The user's home directory — the personal scope root. `null` while it is
	 *  unresolved; every path-checked action is then disabled with a reason. */
	homeDir?: string | null;
	actions: NgwaScopeActions;
	kind?: string;
	onKindChange?: (kind: string) => void;
	search?: string;
	focusScope?: string;
	/** The snapshot is refetching: open menus would act on stale rows. */
	refreshing?: boolean;
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

const MARK_TITLE: Record<CellMark, string> = {
	on: 'enabled here',
	off: 'installed but disabled here',
	link: 'symlinked from the store',
	none: 'not present',
	conflict: 'shadowed by a nearer copy',
	unknown: 'unknown — a source is unreadable',
};

/** What is open. Items are rebuilt from the current rows on every render, so a
 *  refetch or a finished mutation can never leave a stale action clickable. */
interface OpenCell {
	id: string;
	rowKey: string;
	col: string;
	engine: boolean;
}

interface EnableTarget {
	label: string;
	go: () => Promise<unknown>;
}

export function NgwaScopesSurface({
	items,
	isLoading = false,
	error = null,
	unreadableSources = [],
	scopes,
	homeDir = null,
	actions,
	kind = '*',
	onKindChange,
	search,
	focusScope,
	refreshing = false,
}: NgwaScopesSurfaceProps) {
	const [openCell, setOpenCell] = useState<OpenCell | null>(null);
	const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
	const [status, setStatus] = useState<{ tone: 'ok' | 'err'; text: string } | null>(null);
	const [pending, setPending] = useState(false);

	const engines = useMemo(() => installedEngines(items), [items]);
	const engineCols = ENGINE_IDS.filter((e) => engines.has(e));
	const hiddenEngines = ENGINE_IDS.filter((e) => !engines.has(e));

	const orderedScopes = useMemo(() => {
		const personal = scopes.filter((s) => s.key === 'personal');
		const projects = scopes.filter((s) => s.key !== 'personal');
		const rank = (s: ScopeColumn) => (s.key === focusScope ? 0 : s.active ? 1 : 2);
		projects.sort((a, b) => rank(a) - rank(b));
		return [...personal, ...projects];
	}, [scopes, focusScope]);
	const ops = useScopeOps({ items, scopes, homeDir, unreadableSources, actions });
	const {
		allRows,
		conflicts,
		unknownReason,
		scopeLabel,
		rootWhy,
		enableBlock,
		moveSource,
		moveCopyBlock,
		claudeTarget,
		disableBlock,
		engineDisableTarget,
		engineEnableBlock,
		copyRequest,
		moveRequest,
		removeRequest,
		updatePersonalRequest,
		pkgUninstallRequest,
	} = ops;
	const primitivesDown = unreadableSources.filter((s) => PRIMITIVE_SOURCES.includes(s.source));
	const unknown = unknownReason !== undefined;

	const searchedRows = useMemo(() => {
		const q = search?.trim().toLowerCase();
		if (!q) return allRows;
		return allRows.filter(
			(r) => r.name.toLowerCase().includes(q) || r.label.toLowerCase().includes(q)
		);
	}, [allRows, search]);
	const rows = useMemo(
		() => searchedRows.filter((r) => kind === '*' || r.kind === kind),
		[searchedRows, kind]
	);

	const unregistered = useMemo(() => {
		const visible = new Set(orderedScopes.map((s) => s.key));
		const keys = new Set<string>();
		for (const it of items) {
			const k = scopeKeyOf(it.scope);
			if (!visible.has(k)) keys.add(k);
		}
		return [...keys];
	}, [items, orderedScopes]);

	const run = useCallback(async (label: string, fn: () => Promise<unknown>) => {
		setPending(true);
		setStatus(null);
		try {
			await fn();
			setStatus({ tone: 'ok', text: label });
		} catch (e) {
			setStatus({ tone: 'err', text: `${label} failed: ${errText(e)}` });
		} finally {
			setPending(false);
		}
	}, []);

	const closePop = useCallback(() => setOpenCell(null), []);
	const busy = pending || refreshing;

	// ── Popover content ──
	function scopePopItems(row: MatrixRow, col: ScopeColumn): PopItem[] {
		const here = row.byScope.get(col.key);
		const scope = wireOf(col.key);
		const where = col.label;

		if (row.kind === 'schedule' || row.kind === 'workflow') {
			const why =
				row.kind === 'schedule'
					? `Schedules follow their pkg${row.items[0]?.owner_pkg_id ? ` ${row.items[0].owner_pkg_id}` : ''}; enable or disable the pkg instead`
					: 'Workflows have no producer yet (Phase 4)';
			return [
				{ label: 'Enable here', disabledReason: why },
				{ label: 'Move here', disabledReason: why },
				{ label: 'Copy here', disabledReason: why },
				{ sep: true, label: '' },
				{ label: 'Disable', disabledReason: why },
			];
		}

		if (row.pkg) {
			const one = PKG_ONE_SCOPE;
			if (!here) {
				const elsewhere = [...row.byScope.keys()].map(scopeLabel).join(', ');
				return [
					{ label: 'Enable here', disabledReason: `${one}; it is installed in ${elsewhere}` },
					{ label: 'Move here', disabledReason: one },
					{ label: 'Copy here', disabledReason: one },
					{ sep: true, label: '' },
					{ label: 'Disable', disabledReason: 'Not present here' },
					{ label: `Remove from ${where}`, danger: true, disabledReason: 'Not present here' },
				];
			}
			const builtin = here.origin.source === 'builtin';
			return [
				{
					label: 'Enable here',
					disabledReason: here.state === 'enabled' ? 'Already enabled here' : undefined,
					onSelect: () =>
						void run(`Enabled ${row.label}`, () => actions.pkgSetEnabled(here.id, true)),
				},
				{ label: 'Move here', disabledReason: `${one}; it is already here` },
				{ label: 'Copy here', disabledReason: `${one}; it is already here` },
				{ sep: true, label: '' },
				{
					label: 'Disable',
					disabledReason: here.state === 'enabled' ? undefined : 'Already disabled here',
					onSelect: () =>
						void run(`Disabled ${row.label}`, () => actions.pkgSetEnabled(here.id, false)),
				},
				{
					label: `Remove from ${where}`,
					danger: true,
					disabledReason: builtin ? BUILTIN_REMOVE_REASON : undefined,
					onSelect: () => setConfirm(pkgUninstallRequest(row.label, here, where)),
				},
			];
		}

		const sk = row.storeKind;
		if (!sk) return [{ label: 'Enable here', disabledReason: 'This kind has no scope writer' }];
		if (unknownReason) {
			return [
				{ label: 'Enable here', disabledReason: unknownReason },
				{ label: 'Move here', disabledReason: unknownReason },
				{ label: 'Copy here', disabledReason: unknownReason },
				{ sep: true, label: '' },
				{ label: 'Disable', disabledReason: unknownReason },
				{ label: `Remove from ${where}`, danger: true, disabledReason: unknownReason },
			];
		}
		const conflict = conflicts.get(row.key) ?? null;
		if (conflict && col.key === 'personal') return conflictPopItems(conflict);

		const src = moveSource(row, col.key);
		const mcBlock = moveCopyBlock(row, col.key);
		const target = claudeTarget(row, col.key);
		const disableReason = disableBlock(row, col.key);
		return [
			{
				label: 'Enable here',
				disabledReason: enableBlock(row, col.key),
				onSelect: () =>
					void run(`Enabled ${row.label} in ${where}`, () => actions.enable(sk, row.name, scope)),
			},
			{
				label: 'Move here',
				sub: typeof src === 'string' ? undefined : `from ${scopeLabel(src.key)}`,
				disabledReason: mcBlock,
				onSelect: () => typeof src !== 'string' && setConfirm(moveRequest(row, sk, src, col.key)),
			},
			{
				label: 'Copy here',
				sub: typeof src === 'string' ? undefined : `from ${scopeLabel(src.key)}`,
				disabledReason: mcBlock,
				onSelect: () => typeof src !== 'string' && setConfirm(copyRequest(row, sk, src, col.key)),
			},
			{ sep: true, label: '' },
			{
				label: 'Disable',
				disabledReason: disableReason,
				onSelect: () =>
					void run(`Disabled ${row.label} in ${where}`, () => actions.disable(sk, row.name, scope)),
			},
			{
				label: `Remove from ${where}`,
				danger: true,
				disabledReason: typeof target === 'string' ? target : undefined,
				onSelect: () =>
					typeof target !== 'string' && setConfirm(removeRequest(row, sk, col.key, target)),
			},
		];
	}

	function conflictPopItems(c: ScopeConflict): PopItem[] {
		const upd = updatePersonalRequest(c);
		const sk = c.row.storeKind;
		const target = claudeTarget(c.row, 'personal');
		return [
			{
				label: 'Update personal',
				disabledReason: upd
					? undefined
					: !c.project
						? `The shadowing copy (${c.shadowPath}) is not in a registered project`
						: typeof target === 'string'
							? target
							: rootWhy(scopeKeyOf(c.project.scope)),
				onSelect: () => upd && setConfirm(upd),
			},
			{
				label: 'Remove from Personal',
				danger: true,
				disabledReason: typeof target === 'string' ? target : undefined,
				onSelect: () =>
					typeof target !== 'string' &&
					sk &&
					setConfirm(removeRequest(c.row, sk, 'personal', target)),
			},
		];
	}

	function enginePopItems(row: MatrixRow, engine: EngineId): PopItem[] {
		const sk = row.storeKind;
		if (row.pkg || !sk) {
			const why = row.pkg
				? 'A pkg is not placed per engine'
				: row.kind === 'schedule'
					? 'Schedules are not placed per engine'
					: 'This kind has no engine writer';
			return [
				{ label: 'Enable here', disabledReason: why },
				{ label: 'Move here', disabledReason: why },
				{ label: 'Copy here', disabledReason: why },
				{ sep: true, label: '' },
				{ label: 'Disable', disabledReason: why },
			];
		}
		const cross = unknownReason ?? 'Cross-engine move and copy are not wired on this screen';
		const dis = engineDisableTarget(row, engine);
		return [
			{
				label: 'Enable here',
				sub: 'in Personal',
				disabledReason: engineEnableBlock(row, engine),
				onSelect: () =>
					void run(`Enabled ${row.label} for ${engine}`, () =>
						actions.enableFor(engine, sk, row.name, 'workspace')
					),
			},
			{ label: 'Move here', disabledReason: cross },
			{ label: 'Copy here', disabledReason: cross },
			{ sep: true, label: '' },
			{
				label: 'Disable',
				sub: typeof dis === 'string' ? undefined : `in ${scopeLabel(dis.key)}`,
				disabledReason: typeof dis === 'string' ? dis : undefined,
				onSelect: () =>
					typeof dis !== 'string' &&
					void run(`Disabled ${row.label} for ${engine}`, () =>
						actions.disableFor(engine, sk, row.name, wireOf(dis.key))
					),
			},
		];
	}

	function popFor(title: string, list: PopItem[]): OpenPop {
		const items = busy
			? list.map((it) => (it.sep ? it : { ...it, disabledReason: BUSY_REASON }))
			: list;
		return { id: title, title, items };
	}

	// ── Enable all (D-02: confirm, then apply). Targets are exactly the rows
	// whose own cell action is allowed, so "untouched" is true by construction.
	function enableAllScope(col: ScopeColumn): EnableTarget[] {
		const scope = wireOf(col.key);
		const targets: EnableTarget[] = [];
		for (const row of rows) {
			const here = row.byScope.get(col.key);
			if (row.pkg) {
				if (here && here.state !== 'enabled') {
					targets.push({ label: row.label, go: () => actions.pkgSetEnabled(here.id, true) });
				}
			} else if (row.storeKind && enableBlock(row, col.key) === undefined) {
				const sk = row.storeKind;
				targets.push({ label: row.label, go: () => actions.enable(sk, row.name, scope) });
			}
		}
		return targets;
	}
	function enableAllEngine(engine: EngineId): EnableTarget[] {
		const targets: EnableTarget[] = [];
		for (const row of rows) {
			if (row.pkg || !row.storeKind || engineEnableBlock(row, engine) !== undefined) continue;
			const sk = row.storeKind;
			targets.push({
				label: row.label,
				go: () => actions.enableFor(engine, sk, row.name, 'workspace'),
			});
		}
		return targets;
	}
	function enableAllReason(where: string, targets: EnableTarget[]): string | undefined {
		if (unknownReason) return unknownReason;
		if (targets.length === 0) return `Every eligible row is already enabled in ${where}`;
		return undefined;
	}
	// The latest target builders, so a confirmed Enable all re-checks the
	// current rows instead of trusting the list captured when it opened.
	const latest = useRef({ scope: enableAllScope, engine: enableAllEngine });
	latest.current = { scope: enableAllScope, engine: enableAllEngine };

	function askEnableAll(where: string, targets: EnableTarget[], recompute: () => EnableTarget[]) {
		setConfirm({
			title: `Enable all in ${where}`,
			confirmLabel: `Enable ${targets.length}`,
			body: (
				<>
					<p>
						This enables <b>{targets.length}</b> item{targets.length === 1 ? '' : 's'} in <b>{where}</b>.
						Items already enabled there, and anything already on disk at a target path, are untouched.
					</p>
					<p>{targets.map((t) => t.label).join(', ')}</p>
				</>
			),
			run: async () => {
				const now = recompute();
				const was = targets.map((t) => t.label).join('\n');
				if (now.map((t) => t.label).join('\n') !== was) {
					throw new Error('The matrix changed since this was opened; nothing was enabled. Review it again');
				}
				const failed: string[] = [];
				for (const t of now) {
					try {
						await t.go();
					} catch (e) {
						failed.push(`${t.label}: ${errText(e)}`);
					}
				}
				if (failed.length) {
					throw new Error(
						`${targets.length - failed.length} of ${targets.length} enabled; failed — ${failed.join('; ')}`
					);
				}
			},
		});
	}

	// ── Render ──
	if (isLoading) {
		return (
			<div className="view-ngwa flex-1 min-h-0 flex items-center justify-center">
				<span className="text-sm text-muted-foreground" data-snapshot-loading>
					Reading the snapshot… the first rescan can take 1–2 minutes.
				</span>
			</div>
		);
	}

	if (error) {
		return (
			<div className="view-ngwa flex-1 min-h-0 p-4">
				<div className="source-banner" role="alert">
					<AlertTriangle className="h-4 w-4" />
					<span>Failed to load scopes: {error.message}</span>
				</div>
			</div>
		);
	}

	const conflictList = [...conflicts.values()];
	const kindCounts = new Map<string, number>();
	for (const r of searchedRows) kindCounts.set(r.kind, (kindCounts.get(r.kind) ?? 0) + 1);

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			{unreadableSources.length > 0 && (
				<div className="source-banner" role="alert" data-unreadable>
					<AlertTriangle className="h-4 w-4" />
					<span>
						Unreadable:{' '}
						{unreadableSources.map((s) => `${s.source}${s.error ? ` (${s.error})` : ''}`).join('; ')}.
						Rows from those sources are missing, not absent
						{unknown ? ', and actions that place or delete files are disabled' : ''}.
					</span>
				</div>
			)}
			{/* ── Facet Bar ── */}
			<div className="facetbar">
				<div className="frow2">
					<span className="flabel">Kind</span>
					{SCOPE_KINDS.map((k) => {
						const cnt = k.id === '*' ? searchedRows.length : (kindCounts.get(k.id) ?? 0);
						const on = kind === k.id;
						return (
							<button
								key={k.id}
								type="button"
								data-mk={k.id}
								className={`chip ${on ? 'on' : ''}`}
								aria-pressed={on}
								onClick={() => onKindChange?.(k.id)}
							>
								{k.label} <span className="n">{cnt}</span>
							</button>
						);
					})}
					<span className="meta ml-auto" data-mcount>
						{rows.length} items × {orderedScopes.length} scopes × {engineCols.length} engines
					</span>
				</div>
			</div>

			<div className="mwrap">
				<div className="mcol">
					<div className="mscroll sc">
						<table className="matrix" aria-label="Scope and Engine Matrix">
							<thead>
								<tr>
									<th className="left">Equipment</th>
									{orderedScopes.map((col) => {
										const targets = enableAllScope(col);
										const why = enableAllReason(col.label, targets);
										return (
											<th
												key={col.key}
												className={col.active || col.key === focusScope ? 'active' : undefined}
												data-col={col.key}
											>
												<span className="colname">{col.label}</span>
												<span className="colsub">{col.sub}</span>
												<button
													type="button"
													className="chip enall"
													data-enall={col.key}
													disabled={busy || why !== undefined}
													title={busy ? BUSY_REASON : why}
													onClick={() => askEnableAll(col.label, targets, () => latest.current.scope(col))}
												>
													Enable all
												</button>
											</th>
										);
									})}
									{engineCols.map((eng) => {
										const targets = enableAllEngine(eng);
										const why = enableAllReason(eng, targets);
										return (
											<th key={eng} className="eng" data-col={eng}>
												<span className="colname">{eng}</span>
												<span className="colsub">{engines.get(eng)?.version ?? '—'}</span>
												<button
													type="button"
													className="chip enall"
													data-enall={eng}
													disabled={busy || why !== undefined}
													title={busy ? BUSY_REASON : why}
													onClick={() => askEnableAll(eng, targets, () => latest.current.engine(eng))}
												>
													Enable all
												</button>
											</th>
										);
									})}
								</tr>
							</thead>
							<tbody data-mbody>
								{rows.map((row) => {
									const conflict = conflicts.get(row.key) ?? null;
									return (
										<tr key={row.key} data-row={row.key}>
											<td className="item">
												<div className="in">
													{kindIcon(row.kind)}
													<span className="font-medium text-xs">{row.label}</span>
													<span className={`kind k-${row.kind}`}>{row.kind}</span>
												</div>
											</td>
											{orderedScopes.map((col) => {
												const id = `${row.key}|${col.key}`;
												const mark = scopeMark(row, col.key, conflict, unknown);
												const v = row.byScope.get(col.key)?.version;
												return (
													<td key={col.key} className="cell">
														<MatrixCell
															id={id}
															mark={mark}
															version={mark === 'conflict' ? (v ?? '—') : undefined}
															label={`${row.label} in ${col.label}: ${MARK_TITLE[mark]}`}
															open={openCell?.id === id}
															onOpen={() =>
																setOpenCell(
																	openCell?.id === id
																		? null
																		: { id, rowKey: row.key, col: col.key, engine: false }
																)
															}
															pop={
																openCell?.id === id
																	? popFor(`${col.label} · ${row.label}`, scopePopItems(row, col))
																	: null
															}
															onClosePop={closePop}
														/>
													</td>
												);
											})}
											{engineCols.map((eng) => {
												const id = `${row.key}|${eng}`;
												const mark = engineMark(row, eng, unknown);
												return (
													<td key={eng} className="cell eng">
														<MatrixCell
															id={id}
															mark={mark}
															label={`${row.label} for ${eng}: ${MARK_TITLE[mark]}`}
															open={openCell?.id === id}
															onOpen={() =>
																setOpenCell(
																	openCell?.id === id
																		? null
																		: { id, rowKey: row.key, col: eng, engine: true }
																)
															}
															pop={
																openCell?.id === id
																	? popFor(`${eng} · ${row.label}`, enginePopItems(row, eng))
																	: null
															}
															onClosePop={closePop}
														/>
													</td>
												);
											})}
										</tr>
									);
								})}
							</tbody>
						</table>
					</div>

					{status && (
						<div className={`mstatus ${status.tone}`} role="status" data-mstatus>
							{status.text}
						</div>
					)}
					{hiddenEngines.map((eng) => (
						<div className="mfoot" key={eng} data-enginefoot={eng}>
							<Bot className="h-3.5 w-3.5" />
							<span>
								<strong>{eng}</strong> is not installed, so its column is not shown.
							</span>
							<button
								type="button"
								className="chip ml-auto"
								data-act="installengine"
								onClick={actions.openStore}
								title="Opens the Store; engines are listed there"
							>
								Open Store
							</button>
						</div>
					))}
					{unregistered.length > 0 && (
						<div className="mfoot" data-unregistered>
							<AlertTriangle className="h-3.5 w-3.5" />
							<span>
								Some items sit in scopes that are not registered projects and have no column:{' '}
								{unregistered.join(', ')}
							</span>
						</div>
					)}
				</div>

				{/* ── Sidenote ── */}
				<aside className="sidenote sc" data-sidenote aria-label="Matrix precedence and legend">
					<div className="subhead first">Precedence</div>
					{unknown ? (
						<div className="snote warn" data-conflicts-unknown>
							Conflicts unknown: {primitivesDown.map((s) => s.source).join(', ')} unreadable.
						</div>
					) : conflictList.length > 0 ? (
						conflictList.map((c) => {
							const projKey = c.project ? scopeKeyOf(c.project.scope) : null;
							const upd = updatePersonalRequest(c);
							const target = claudeTarget(c.row, 'personal');
							return (
								<div key={c.row.key} className="conflictbox" data-conflict={c.row.key}>
									<AlertTriangle className="h-4 w-4 flex-none" />
									<div className="txt">
										<span className="t1">{c.row.label} exists twice</span>
										<span className="t2">
											personal <code>{c.personal.version ?? '—'}</code> ·{' '}
											{projKey ? scopeLabel(projKey) : c.shadowPath}{' '}
											<code>{c.project?.version ?? '—'}</code>. The nearer scope wins, so inside{' '}
											{projKey ? scopeLabel(projKey) : 'that project'} every session loads the project
											copy and the personal copy is shadowed.
										</span>
										<div className="acts">
											<button
												type="button"
												className="chip"
												data-act="update-personal"
												disabled={!upd}
												title={
													upd
														? undefined
														: typeof target === 'string'
															? target
															: 'The shadowing copy is not in a registered project'
												}
												onClick={() => upd && setConfirm(upd)}
											>
												Update personal
											</button>
											<button
												type="button"
												className="chip clear"
												data-act="remove-personal"
												disabled={typeof target === 'string' || !c.row.storeKind}
												title={typeof target === 'string' ? target : undefined}
												onClick={() =>
													typeof target !== 'string' &&
													c.row.storeKind &&
													setConfirm(removeRequest(c.row, c.row.storeKind, 'personal', target))
												}
											>
												Remove personal
											</button>
										</div>
									</div>
								</div>
							);
						})
					) : (
						<div className="snote" data-no-conflicts>
							No precedence conflicts: no personal item is shadowed by a project copy.
						</div>
					)}

					<div className="subhead">Reading a cell</div>
					<div className="legend">
						<div>
							<CircleDot className="h-3.5 w-3.5 mk-on" />
							<span>enabled in this scope</span>
						</div>
						<div>
							<Circle className="h-3.5 w-3.5" />
							<span>installed but disabled</span>
						</div>
						<div>
							<ArrowRight className="h-3.5 w-3.5 mk-link" />
							<span>symlinked from the store, not copied</span>
						</div>
						<div>
							<Minus className="h-3.5 w-3.5" />
							<span>not present here</span>
						</div>
						<div>
							<AlertTriangle className="h-3.5 w-3.5 mk-conflict" />
							<span>shadowed by a nearer copy of the same name</span>
						</div>
						<div>
							<HelpCircle className="h-3.5 w-3.5" />
							<span>unknown: its source is unreadable</span>
						</div>
					</div>
					<p className="note">
						Click any cell to enable, move or copy the item into that scope or engine. Column headers
						act on the whole column.
					</p>
				</aside>
			</div>

			<NgwaConfirmDialog
				request={confirm}
				onClose={(result) => {
					const title = confirm?.title ?? '';
					setConfirm(null);
					setOpenCell(null);
					if (result?.ok) setStatus({ tone: 'ok', text: `${title}: done` });
					else if (result && !result.ok)
						setStatus({ tone: 'err', text: `${title} failed: ${result.error}` });
				}}
			/>
		</div>
	);
}

function MarkIcon({ mark }: { mark: CellMark }) {
	switch (mark) {
		case 'on':
			return <CircleDot className="h-3.5 w-3.5" />;
		case 'off':
			return <Circle className="h-3.5 w-3.5" />;
		case 'link':
			return <ArrowRight className="h-3.5 w-3.5" />;
		case 'conflict':
			return <AlertTriangle className="h-3.5 w-3.5" />;
		case 'unknown':
			return <HelpCircle className="h-3.5 w-3.5" />;
		default:
			return <Minus className="h-3 w-3" />;
	}
}

function MatrixCell({
	id,
	mark,
	version,
	label,
	open,
	onOpen,
	pop,
	onClosePop,
}: {
	id: string;
	mark: CellMark;
	version?: string;
	label: string;
	open: boolean;
	onOpen: () => void;
	pop: OpenPop | null;
	onClosePop: () => void;
}) {
	const btnRef = useRef<HTMLButtonElement>(null);
	return (
		<div className="cellwrap">
			<button
				ref={btnRef}
				type="button"
				className={`cellbtn ${mark === 'none' ? 'no' : mark} ${open ? 'open' : ''}`}
				data-cell={id}
				data-mark={mark}
				aria-haspopup="menu"
				aria-expanded={open}
				aria-label={label}
				title={label}
				onClick={onOpen}
			>
				<MarkIcon mark={mark} />
				{version !== undefined && <span className="cellver">{version}</span>}
			</button>
			{pop && <NgwaPopMenu pop={pop} anchor={btnRef} onClose={onClosePop} />}
		</div>
	);
}
