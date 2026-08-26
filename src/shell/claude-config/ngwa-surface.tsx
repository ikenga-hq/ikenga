// Ngwa surfaces (WP-07) — the read-WRITE Claude-config manager.
//
// This is the SUPERSET of the read-only /claude browser: it ports the rich
// detail (body/script preview, supporting-files tree, typed relationship
// chips, precedence/override signal) and decorates it with write actions
// (enable/disable · move · copy · remove/delete · import/install · reveal).
//
// Two MANAGE surfaces share one data model + one detail pane (D-02):
//   • Browse   — 2-pane (list │ resizable divider │ detail superset).
//   • Registry — full-width 8-col table + @filter DSL + bulk actions; detail
//                opens in a right-sliding drawer that reuses the same superset.
// ANALYZE surfaces (graph/map/life/health/flow) are Phase 4 — placeholder.
//
// Reads `?surface=&scope=&kind=` (threaded by WP-06's Ngwa sidebar) to pick
// the surface, scope filter, and kind filter. All reads/writes go through the
// WP-05 hooks in `lib/queries/claude-config.ts` (mock-backed in dev).
//
// Design refs: designs/cockpit-shell-v2-mode.html (D-02) +
//              designs/cockpit-ngwa-hifi.html (D-05).

import { useEffect, useMemo, useRef, useState } from 'react';
import { useQuery } from '@tanstack/react-query';

import { cn } from '@/components/ui/utils';
import { useFocusTrap } from '@/lib/a11y/focus';
import { Markdown } from '@/components/markdown';
import { shortPath } from '@/lib/home';
import {
	claudeConfigReadFile,
	type ClaudeAgent,
	type ClaudeCommand,
	type ClaudeConfig,
	type ClaudeHook,
	type ClaudeMcp,
	type ClaudeSkill,
	type ClaudeStoreEntry,
	type ClaudeStoreKind,
	type ClaudeStoreScope,
	type Project,
} from '@/lib/tauri-cmd';
import {
	claudeStoreQueryOptions,
	obaDependentsQueryOptions,
	useBackfillRegistry,
	useCopyPrimitive,
	useDisablePrimitive,
	useEnablePrimitive,
	useImportToStore,
	useMovePrimitive,
	useObaAutoUpdateOnMount,
	useObaInstallWithDeps,
	useObaSetAutoUpdate,
	useObaUpdate,
	useRelinkDependents,
	useRemovePrimitive,
	type ConfigFormat,
} from '@/lib/queries/claude-config';
import { obaSafeDelete, type SafeDeleteOutcome } from '@/lib/tauri-cmd';
import {
	mergePrimitiveView,
	resolveCatalogClosure,
	usePrimitiveCatalog,
	PRIMITIVE_STATUS_WORD,
	type ConsentDep,
	type PrimitiveCatalogEntry,
	type PrimitiveStatus,
	type PrimitiveViewItem,
} from '@/lib/registry/primitives';

import { Chips, FrontmatterGrid } from './list-detail';
import { AnalyzeSurface } from './analyze';
import { EngineGlyph } from './engine-glyph';
import { WriteTranscodeDrawer, isCrossEngineCopyable } from './write-drawer';

export * from './ngwa-helpers';
import {
	ENGINE_META,
	ENGINE_ORDER,
	UI_KIND_OF,
	buildItems,
	resolveActiveSystems,
	siblingSystemsOf,
	summarizeSystems,
	type ItemState,
	type NgwaItem,
	type NgwaKindId,
	type NgwaScopeId,
	type NgwaSurfaceId,
	type NgwaSystemId,
} from './ngwa-helpers';

// Map a serialization format onto the short row chip + its tint class.
const FORMAT_CHIP: Record<ConfigFormat, { label: string; cls: 'json' | 'toml' | 'md' }> = {
	'json-embedded': { label: 'json', cls: 'json' },
	toml: { label: 'toml', cls: 'toml' },
	'md-yaml': { label: 'md', cls: 'md' },
};
// Longer human label for the detail-pane Identity grid.
const FORMAT_LABEL: Record<ConfigFormat, string> = {
	'json-embedded': 'JSON (settings-embedded)',
	toml: 'TOML',
	'md-yaml': 'Markdown + YAML frontmatter',
};

const ANALYZE: ReadonlySet<NgwaSurfaceId> = new Set<NgwaSurfaceId>([
	'graph',
	'map',
	'life',
	'health',
	'flow',
]);

const ANALYZE_LABEL: Record<string, string> = {
	graph: 'Capability graph',
	map: 'Store map',
	life: 'Hook lifecycle',
	health: 'Inventory & health',
	flow: 'Orchestration flow',
};

// ─── Unified item model ─────────────────────────────────────────────────────
// Browse + Registry render off one normalized shape derived from the on-disk
const KIND_LABEL: Record<NgwaKindId, string> = {
	skills: 'Skills',
	agents: 'Agents',
	commands: 'Commands',
	hooks: 'Hooks',
	mcps: 'MCP',
};
const KIND_ABBR: Record<NgwaKindId, string> = {
	skills: 'skill',
	agents: 'agent',
	commands: 'cmd',
	hooks: 'hook',
	mcps: 'mcp',
};
const STATE_WORD: Record<ItemState, string> = {
	enabled: 'Enabled',
	disabled: 'Disabled',
	local: 'Local',
	orphaned: 'Orphaned',
	linked: 'Linked',
};

// ─── Top-level surface ──────────────────────────────────────────────────────

// ─── Top-level surface ──────────────────────────────────────────────────────

interface NgwaSurfaceProps {
	config: ClaudeConfig | null;
	isLoading: boolean;
	error: string | null;
	surface: NgwaSurfaceId;
	scope: NgwaScopeId;
	kind: NgwaKindId;
	/** Selected systems from the URL `sys` param. Empty/absent ⇒ all present. */
	systems: NgwaSystemId[];
	onEdit: (path: string) => void;
	/** Available project scopes to offer in move/copy/install pickers. */
	projectScopes: Array<{ key: ClaudeStoreScope; label: string }>;
	/** DB project rows — used to map a scanned project root to its real store id. */
	projects: Project[];
}

export function NgwaSurface({
	config,
	isLoading,
	error,
	surface,
	scope,
	kind,
	systems,
	onEdit,
	projectScopes,
	projects,
}: NgwaSurfaceProps) {
	const storeQuery = useQuery(claudeStoreQueryOptions(null));
	const store = storeQuery.data ?? [];

	const items = useMemo(
		() => (config ? buildItems(config, store, projects) : []),
		[config, store, projects]
	);

	// SYSTEM facet — engines present + the active (toggled-on) subset. The active
	// set drives both the engine-grouped Browse rows and the aggregated counts.
	const summary = useMemo(() => summarizeSystems(items), [items]);
	const activeSystems = useMemo(
		() => resolveActiveSystems(systems, summary.present),
		[systems, summary.present]
	);

	// Total reflects the active systems too, so the header count tracks the facet.
	const total = useMemo(
		() => items.reduce((n, it) => (activeSystems.has(it.system) ? n + 1 : n), 0),
		[items, activeSystems]
	);

	const isAnalyze = ANALYZE.has(surface);
	const isStore = surface === 'store';

	return (
		<div className="ccfg ngwa-fill" style={{ gridTemplateRows: 'auto 1fr auto' }}>
			<div className="ngwa-hd">
				<div className="ttl">
					<h1>
						<span className="glyph">⌗</span> Ngwa
					</h1>
					<span className="ct">{total} entries</span>
					<span className="sub">
						{isAnalyze
							? ANALYZE_LABEL[surface]
							: isStore
								? 'Ọba · canonical store · install into scopes'
								: surface === 'registry'
									? 'one registry · all primitives · all scopes · bulk triage'
									: 'agents · skills · commands · hooks · MCP, across scopes'}
					</span>
				</div>
				<div className="hd-right">
					{/* engine seam — live badge set of the active systems (D-08). The
					    SYSTEM facet in the sidebar is the toggle; this is a read-only
					    indicator of which engines are currently in view. */}
					{summary.present.length > 1 && (
						<div
							className="ngwa-sysset"
							title="Systems in view — toggle in the sidebar SYSTEM facet"
						>
							{summary.present.map((e) => (
								<span
									key={e}
									className={cn('ngwa-eb', ENGINE_META[e].code, !activeSystems.has(e) && 'off')}
									title={ENGINE_META[e].display}
								>
									<EngineGlyph system={e} />
								</span>
							))}
						</div>
					)}
				</div>
			</div>

			{isAnalyze ? (
				<AnalyzeSurface
					surface={surface}
					config={config}
					scope={scope}
					items={items}
					store={store}
				/>
			) : isStore ? (
				<StoreSurface
					store={store}
					isLoading={isLoading || storeQuery.isLoading}
					error={error}
					onEdit={onEdit}
					projectScopes={projectScopes}
				/>
			) : surface === 'registry' ? (
				<RegistrySurface
					items={items}
					store={store}
					scope={scope}
					activeSystems={activeSystems}
					present={summary.present}
					isLoading={isLoading || storeQuery.isLoading}
					error={error}
					onEdit={onEdit}
					projectScopes={projectScopes}
				/>
			) : (
				<BrowseSurface
					items={items}
					scope={scope}
					kind={kind}
					activeSystems={activeSystems}
					present={summary.present}
					isLoading={isLoading}
					error={error}
					onEdit={onEdit}
					projectScopes={projectScopes}
				/>
			)}

			<Legend />
		</div>
	);
}

function Legend() {
	return (
		<div className="ngwa-legend">
			<span>
				<i className="ngwa-dot enabled" /> enabled
			</span>
			<span>
				<i className="ngwa-dot disabled" /> disabled
			</span>
			<span>
				<i className="ngwa-dot local" /> local · not in store
			</span>
			<span>
				<i className="ngwa-dot linked" /> linked · external master
			</span>
			<span>
				<i className="ngwa-dot orphaned" /> orphaned
			</span>
			<span style={{ marginLeft: 'auto' }}>
				<i className="ngwa-mtag link">link</i> symlink → store
			</span>
			<span>
				<i className="ngwa-mtag merge">merge</i> JSON-spliced into settings
			</span>
			<span>
				<i className="ngwa-ovr">ovr</i> shadowed by higher scope
			</span>
		</div>
	);
}

// ─── Scope filtering shared by both surfaces ────────────────────────────────
function passScope(it: NgwaItem, scope: NgwaScopeId): boolean {
	if (scope === 'all') return true;
	if (scope === 'personal') return it.scope === 'personal';
	// `project:<id>` — narrow to the one project this item belongs to.
	return it.scopeKey === scope;
}

// ─── Shared 2-pane resizable divider (G-06) ─────────────────────────────────
// Drag-to-resize the list│detail split + double-click to reset. Shared by the
// Browse and Store surfaces, which use the same `.ccfg-split` layout.
//
// Keyboard alternative to the drag (WCAG 2.5.7 Dragging Movements): the divider
// is a focusable `role="separator"` — ArrowLeft/Right nudge ±8px, Home resets
// to the default, End snaps to max. `aria-valuenow/min/max` are kept in sync
// imperatively so AT announces the current split width.
const SPLIT_MIN_W = 220;
const SPLIT_DETAIL_MIN = 360 + 4; // detail pane min + divider gutter
function useResizableSplit(
	splitRef: React.RefObject<HTMLDivElement | null>,
	dividerRef: React.RefObject<HTMLDivElement | null>
) {
	useEffect(() => {
		const split = splitRef.current;
		const divider = dividerRef.current;
		if (!split || !divider) return;

		// Reflect the current geometry onto the separator's aria-value* so AT can
		// announce the split. Called after every resize (drag or keyboard).
		function syncAria() {
			if (!split || !divider) return;
			const rect = split.getBoundingClientRect();
			const listW =
				(split.firstElementChild as HTMLElement | null)?.getBoundingClientRect().width ?? 0;
			const max = Math.max(SPLIT_MIN_W, rect.width - SPLIT_DETAIL_MIN);
			divider.setAttribute('aria-valuemin', String(SPLIT_MIN_W));
			divider.setAttribute('aria-valuemax', String(Math.round(max)));
			divider.setAttribute('aria-valuenow', String(Math.round(listW)));
		}

		function clampW(next: number): number {
			if (!split) return next;
			const rect = split.getBoundingClientRect();
			const max = rect.width - SPLIT_DETAIL_MIN;
			if (next < SPLIT_MIN_W) return SPLIT_MIN_W;
			if (next > max) return max;
			return next;
		}

		function onDown(e: MouseEvent) {
			if (!split) return;
			e.preventDefault();
			const rect = split.getBoundingClientRect();
			const startX = e.clientX;
			const startListW =
				(split.firstElementChild as HTMLElement | null)?.getBoundingClientRect().width ?? 300;
			function onMove(ev: MouseEvent) {
				if (!split) return;
				const delta = ev.clientX - startX;
				let next = startListW + delta;
				const max = rect.width - SPLIT_DETAIL_MIN;
				if (next < SPLIT_MIN_W) next = SPLIT_MIN_W;
				if (next > max) next = max;
				split.style.setProperty('--list-w', `${next}px`);
				syncAria();
			}
			function onUp() {
				document.removeEventListener('mousemove', onMove);
				document.removeEventListener('mouseup', onUp);
			}
			document.addEventListener('mousemove', onMove);
			document.addEventListener('mouseup', onUp);
		}
		function onDouble() {
			split?.style.removeProperty('--list-w');
			syncAria();
		}
		function onKeyDown(e: KeyboardEvent) {
			if (!split) return;
			const curW =
				(split.firstElementChild as HTMLElement | null)?.getBoundingClientRect().width ?? 300;
			if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
				e.preventDefault();
				const delta = e.key === 'ArrowRight' ? 8 : -8;
				split.style.setProperty('--list-w', `${clampW(curW + delta)}px`);
				syncAria();
			} else if (e.key === 'Home') {
				e.preventDefault();
				split.style.removeProperty('--list-w');
				syncAria();
			} else if (e.key === 'End') {
				e.preventDefault();
				const rect = split.getBoundingClientRect();
				split.style.setProperty('--list-w', `${clampW(rect.width - SPLIT_DETAIL_MIN)}px`);
				syncAria();
			}
		}

		divider.addEventListener('mousedown', onDown);
		divider.addEventListener('dblclick', onDouble);
		divider.addEventListener('keydown', onKeyDown);
		// Initial + on-resize aria sync so the announced value is correct before
		// any interaction.
		syncAria();
		const ro = new ResizeObserver(syncAria);
		ro.observe(split);
		return () => {
			divider.removeEventListener('mousedown', onDown);
			divider.removeEventListener('dblclick', onDouble);
			divider.removeEventListener('keydown', onKeyDown);
			ro.disconnect();
		};
	}, [splitRef, dividerRef]);
}

// ─── BROWSE surface (2-pane: list │ resizable divider │ detail) ─────────────

interface BrowseProps {
	items: NgwaItem[];
	scope: NgwaScopeId;
	kind: NgwaKindId;
	activeSystems: ReadonlySet<NgwaSystemId>;
	present: NgwaSystemId[];
	isLoading: boolean;
	error: string | null;
	onEdit: (path: string) => void;
	projectScopes: Array<{ key: ClaudeStoreScope; label: string }>;
}

function BrowseSurface({
	items,
	scope,
	kind,
	activeSystems,
	present,
	isLoading,
	error,
	onEdit,
	projectScopes,
}: BrowseProps) {
	const [filter, setFilter] = useState('');
	const [selId, setSelId] = useState<string | null>(null);

	const splitRef = useRef<HTMLDivElement | null>(null);
	const dividerRef = useRef<HTMLDivElement | null>(null);
	useResizableSplit(splitRef, dividerRef);

	const list = useMemo(() => {
		let xs = items.filter(
			(it) => it.uiKind === kind && passScope(it, scope) && activeSystems.has(it.system)
		);
		if (filter.trim()) {
			const f = filter.toLowerCase();
			xs = xs.filter(
				(it) =>
					it.name.toLowerCase().includes(f) || (it.description ?? '').toLowerCase().includes(f)
			);
		}
		return xs;
	}, [items, kind, scope, filter, activeSystems]);

	const selected = useMemo(() => {
		if (!list.length) return null;
		return list.find((it) => it.id === selId) ?? list[0];
	}, [list, selId]);

	// Engine-grouped clusters (D-08): rows cluster under a tinted per-engine
	// sub-header rather than interleaving. Only emitted when more than one engine
	// is active — a single active engine collapses to the flat list so the
	// Claude-only view is byte-identical to today (regression guard).
	const engineGroups = useMemo<Array<{ system: NgwaSystemId; items: NgwaItem[] }> | null>(() => {
		const activeCount = present.filter((e) => activeSystems.has(e)).length;
		if (activeCount <= 1) return null;
		const by = new Map<NgwaSystemId, NgwaItem[]>();
		for (const it of list) {
			if (!by.has(it.system)) by.set(it.system, []);
			by.get(it.system)!.push(it);
		}
		// Stable CL→GM→CX order; only engines that actually have rows.
		return ENGINE_ORDER.filter((e) => by.has(e)).map((e) => ({ system: e, items: by.get(e)! }));
	}, [list, present, activeSystems]);

	// A short path hint for an engine sub-header, derived from the first row's
	// on-disk path (e.g. `.gemini/agents`). Falls back to the engine display.
	function enginePathHint(its: NgwaItem[]): string {
		const p = its[0]?.path ?? '';
		const m = p.match(/(\.(?:claude|gemini|codex)\/[a-z]+)/);
		return m ? m[1] : ENGINE_META[its[0]?.system ?? 'claude'].display;
	}

	return (
		<div className="ccfg-split" ref={splitRef} style={{ minHeight: 0 }}>
			<div className="ccfg-list">
				<div className="ccfg-list-toolbar">
					<div className="ccfg-search-wrap">
						<svg
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							strokeWidth={2}
							aria-hidden="true"
						>
							<circle cx="11" cy="11" r="7" />
							<path d="m20 20-3-3" />
						</svg>
						<input
							className="ccfg-search"
							value={filter}
							onChange={(e) => setFilter(e.target.value)}
							placeholder="filter…"
							aria-label="Filter by name or description"
						/>
					</div>
				</div>
				<div className="ccfg-list-meta">
					<span>{KIND_LABEL[kind].toUpperCase()}</span>
					<span>
						{scope === 'all' ? 'all scopes' : scope.startsWith('project:') ? scope.slice(8) : scope}{' '}
						· {list.length}
						{engineGroups && ` · ${engineGroups.length} systems`}
					</span>
				</div>
				<div className="ccfg-list-rows">
					{isLoading ? (
						<div className="ccfg-empty">Loading…</div>
					) : error ? (
						<div className="ccfg-empty">{error}</div>
					) : list.length === 0 ? (
						<div className="ccfg-empty">No entries match.</div>
					) : engineGroups ? (
						engineGroups.map(({ system, items: its }) => (
							<div key={system}>
								<div className={cn('ccfg-engine-head', ENGINE_META[system].code)}>
									<span className="eg" aria-hidden>
										<EngineGlyph system={system} />
									</span>
									<span className="hn">
										{ENGINE_META[system].display} · {its.length}
									</span>
									<span className="ct">{enginePathHint(its)}</span>
								</div>
								{its.map((it) => (
									<BrowseRow
										key={it.id}
										item={it}
										active={!!selected && selected.id === it.id}
										onClick={() => setSelId(it.id)}
										showEngineMeta
									/>
								))}
							</div>
						))
					) : (
						list.map((it) => (
							<BrowseRow
								key={it.id}
								item={it}
								active={!!selected && selected.id === it.id}
								onClick={() => setSelId(it.id)}
							/>
						))
					)}
				</div>
			</div>
			{/* Presentational mouse-resize handle (drag wired via the ref). No keyboard
			    path → no ARIA splitter role; overclaiming role="separator" on a
			    non-focusable, value-prop-less handle is worse than decorative. */}
			<div className="ccfg-divider" ref={dividerRef} aria-hidden="true" />
			<div className="ccfg-detail ngwa-detail">
				{selected ? (
					<ItemDetail
						item={selected}
						projectScopes={projectScopes}
						onEdit={onEdit}
						siblingSystems={siblingSystemsOf(selected, items)}
						present={present}
					/>
				) : (
					<EmptyCarve />
				)}
			</div>
		</div>
	);
}

// ─── STORE surface (Ọba — 2-pane: list │ divider │ detail) ──────────────────
// Promoted from the old `kind=store` branch of Browse into its own MANAGE
// surface (peer to Browse/Registry). The catalog spans all kinds + engines, so
// the sidebar KIND + SYSTEM facets dim here (handled in ngwa-mode.tsx).
//
// WP-10a: the local store (what's installed) is merged with the primitive
// catalog (what's available to install) into one status-tagged list, sliced by
// the Installed | Available | Updatable filter chips. Install/Update actions
// are disabled-pending Phase 2 (git/npx install + update, WP-07–09).

interface StoreProps {
	store: ClaudeStoreEntry[];
	isLoading: boolean;
	error: string | null;
	onEdit: (path: string) => void;
	projectScopes: Array<{ key: ClaudeStoreScope; label: string }>;
}

type StoreStatusFilter = 'all' | PrimitiveStatus;

const STORE_CHIPS: Array<{ id: StoreStatusFilter; label: string }> = [
	{ id: 'all', label: 'All' },
	{ id: 'installed', label: 'Installed' },
	{ id: 'available', label: 'Available' },
	{ id: 'updatable', label: 'Updatable' },
];

function StoreSurface({ store, isLoading, error, onEdit, projectScopes }: StoreProps) {
	const [filter, setFilter] = useState('');
	const [statusFilter, setStatusFilter] = useState<StoreStatusFilter>('all');
	const [selId, setSelId] = useState<string | null>(null);

	const splitRef = useRef<HTMLDivElement | null>(null);
	const dividerRef = useRef<HTMLDivElement | null>(null);
	useResizableSplit(splitRef, dividerRef);

	// Ọba registry back-fill — scan the live config farm for EXTERNAL masters
	// (symlinks resolving to a master outside the store, e.g. groundwork) and
	// register their provenance. It doesn't move files into the store, so the
	// `canonical` count is unchanged; the win is provenance shown in each
	// `linked` item's detail. Surface the returned count as feedback.
	const backfill = useBackfillRegistry();
	const [backfillMsg, setBackfillMsg] = useState<string | null>(null);

	// WP-10a: recommendation feed. Bundled seed for now (remote signed catalog
	// is WP-10b). Merge with the local store → status-tagged rows.
	const catalogQuery = usePrimitiveCatalog();
	const catalog = catalogQuery.data ?? [];
	const view = useMemo(() => mergePrimitiveView(store, catalog), [store, catalog]);

	// Phase 3 — auto-update trust policy (catalog ON, manual OFF). On mount we
	// sweep every `autoUpdate`-opted entry and re-fetch the stale ones (FE-driven,
	// NOT a background daemon). The same mutation backs the explicit "Check
	// updates" button below.
	const autoSweep = useObaAutoUpdateOnMount(true);
	const sweepMsg = autoSweep.isPending
		? 'Checking for updates…'
		: autoSweep.data
			? autoSweep.data.updated.length > 0
				? `Auto-updated ${autoSweep.data.updated.length} catalog ${autoSweep.data.updated.length === 1 ? 'primitive' : 'primitives'}.`
				: autoSweep.data.errored.length > 0
					? `Up to date (${autoSweep.data.errored.length} check${autoSweep.data.errored.length === 1 ? '' : 's'} errored).`
					: null
			: null;

	const rows = useMemo(() => {
		let xs = statusFilter === 'all' ? view : view.filter((r) => r.status === statusFilter);
		if (filter.trim()) {
			const f = filter.toLowerCase();
			xs = xs.filter(
				(r) => r.name.toLowerCase().includes(f) || (r.description ?? '').toLowerCase().includes(f)
			);
		}
		return xs;
	}, [view, statusFilter, filter]);

	const counts = useMemo(() => {
		const c = { installed: 0, available: 0, updatable: 0 } as Record<PrimitiveStatus, number>;
		for (const r of view) c[r.status] += 1;
		return c;
	}, [view]);

	const selected = useMemo(() => {
		if (!rows.length) return null;
		return rows.find((r) => r.key === selId) ?? rows[0];
	}, [rows, selId]);

	const loading = isLoading || catalogQuery.isLoading;
	// Nothing installed AND nothing recommended — a genuinely empty surface.
	const allEmpty = !loading && !error && view.length === 0;

	return (
		<div className="ccfg-split" ref={splitRef} style={{ minHeight: 0 }}>
			<div className="ccfg-list">
				<div className="ccfg-list-toolbar">
					<div className="ccfg-search-wrap">
						<svg
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							strokeWidth={2}
							aria-hidden="true"
						>
							<circle cx="11" cy="11" r="7" />
							<path d="m20 20-3-3" />
						</svg>
						<input
							className="ccfg-search"
							value={filter}
							onChange={(e) => setFilter(e.target.value)}
							placeholder="filter catalog…"
							aria-label="Filter catalog entries"
						/>
					</div>
					<button
						type="button"
						className="ngwa-btn"
						disabled={backfill.isPending}
						title="Scan the live config farm for external masters (e.g. groundwork) and register their provenance into the Ọba registry."
						onClick={() => {
							setBackfillMsg(null);
							backfill.mutate(undefined, {
								onSuccess: (n) =>
									setBackfillMsg(
										n > 0
											? `Back-filled ${n} external master${n === 1 ? '' : 's'}.`
											: 'No new external masters found.'
									),
								onError: (e) => setBackfillMsg(String(e)),
							});
						}}
					>
						{backfill.isPending ? 'Backfilling…' : 'Backfill'}
					</button>
					<button
						type="button"
						className="ngwa-btn"
						disabled={autoSweep.isPending}
						title="Re-check every auto-update primitive (curated catalog installs) against its remote and refresh the stale ones in place."
						onClick={() => autoSweep.mutate()}
					>
						{autoSweep.isPending ? 'Checking…' : 'Check updates'}
					</button>
				</div>
				<div className="ngwa-toolbar" style={{ borderTop: 0, paddingTop: 0 }}>
					{STORE_CHIPS.map((c) => {
						const n = c.id === 'all' ? view.length : counts[c.id];
						return (
							<button
								key={c.id}
								type="button"
								className={cn('ngwa-fchip', statusFilter === c.id && 'on')}
								onClick={() => setStatusFilter(c.id)}
							>
								{c.label}
								{c.id !== 'all' && <span style={{ opacity: 0.6 }}> {n}</span>}
							</button>
						);
					})}
				</div>
				<div className="ccfg-list-meta">
					<span>STORE / CATALOG</span>
					<span role="status" aria-live="polite" aria-atomic="true">
						{backfillMsg ??
							sweepMsg ??
							`${counts.installed} installed · ${counts.available} available`}
					</span>
				</div>
				<div className="ccfg-list-rows">
					{loading ? (
						<div className="ccfg-empty">Loading…</div>
					) : error ? (
						<div className="ccfg-empty">{error}</div>
					) : allEmpty ? (
						<div className="ccfg-empty">
							Ọba store is empty — nothing installed or recommended yet.
						</div>
					) : rows.length === 0 ? (
						<div className="ccfg-empty">No entries match this filter.</div>
					) : (
						rows.map((r) => (
							<PrimitiveRow
								key={r.key}
								item={r}
								active={selected != null && selected.key === r.key}
								onClick={() => setSelId(r.key)}
							/>
						))
					)}
				</div>
			</div>
			{/* Presentational mouse-resize handle (drag wired via the ref). No keyboard
			    path → no ARIA splitter role; overclaiming role="separator" on a
			    non-focusable, value-prop-less handle is worse than decorative. */}
			<div className="ccfg-divider" ref={dividerRef} aria-hidden="true" />
			<div className="ccfg-detail ngwa-detail">
				{selected?.store ? (
					<StoreDetail
						entry={selected.store}
						projectScopes={projectScopes}
						onEdit={onEdit}
						updatable={selected.status === 'updatable'}
						catalogVersion={selected.catalog?.version ?? null}
					/>
				) : selected?.catalog ? (
					<CatalogDetail entry={selected.catalog} catalog={catalog} store={store} />
				) : allEmpty ? (
					<div className="ccfg-empty">
						<div className="carve">▽▽▽</div>
						The Ọba store has no entries yet.
						<br />
						Import a primitive from Browse, or install one from the catalog.
					</div>
				) : (
					<EmptyCarve />
				)}
			</div>
		</div>
	);
}

// One row in the merged store view — installed canonical, an available catalog
// recommendation, or an installed-but-updatable entry.
function PrimitiveRow({
	item,
	active,
	onClick,
}: {
	item: PrimitiveViewItem;
	active: boolean;
	onClick: () => void;
}) {
	const dot: ItemState =
		item.status === 'available' ? 'disabled' : item.status === 'updatable' ? 'linked' : 'enabled';
	const installedScopes = item.store?.enabledIn.length ?? 0;
	return (
		<button
			type="button"
			onClick={onClick}
			className={cn('ccfg-row text-left w-full', active && 'is-on')}
		>
			<div className="ccfg-row-name">
				<span
					className={cn('ngwa-dot', dot)}
					title={PRIMITIVE_STATUS_WORD[item.status]}
					aria-hidden
				/>
				<span>{item.name}</span>
				<span className="ccfg-scope">{KIND_ABBR[UI_KIND_OF[item.kind]]}</span>
				{item.status === 'updatable' && item.catalog && (
					<span className="ngwa-ovr" title={`Catalog has ${item.catalog.version}`}>
						upd
					</span>
				)}
			</div>
			{item.description && <div className="ccfg-row-desc">{item.description}</div>}
			<div className="ccfg-row-meta">
				<span className={cn('ngwa-stword', item.status === 'available' ? 'disabled' : 'enabled')}>
					{item.status === 'updatable'
						? `Update → ${item.catalog?.version}`
						: item.status === 'available'
							? 'Available'
							: installedScopes
								? `Installed · ${installedScopes}`
								: 'In store'}
				</span>
				<span>·</span>
				<span>{item.status === 'available' ? `via ${item.catalog?.source}` : 'canonical'}</span>
			</div>
		</button>
	);
}

// Detail for a catalog-only (available) recommendation. Install resolves to a
// git/npx fetch into the vault (Ọba Phase 2 · WP-07/08); placement into a scope
// is the separate per-scope Enable step on the resulting store entry.
// Tauri command rejections surface as plain strings (the `Err(String)` body),
// not `Error` objects — so `.message` is undefined. Coerce either shape.
function errText(e: unknown): string {
	return e instanceof Error ? e.message : String(e);
}

function CatalogDetail({
	entry,
	catalog,
	store,
}: {
	entry: PrimitiveCatalogEntry;
	catalog: PrimitiveCatalogEntry[];
	store: ClaudeStoreEntry[];
}) {
	const install = useObaInstallWithDeps();
	const isFileBased = entry.kind === 'skill' || entry.kind === 'agent' || entry.kind === 'command';

	// WP-15 consent surface. The catalog carries `requires`, so we compute the
	// forward-dependency closure HERE (no fetch) to show the user "also installs…"
	// BEFORE the install. The Rust `oba_install_with_deps` resolver re-derives the
	// closure authoritatively at install time; this is consent display only.
	const installedKeys = useMemo(() => new Set(store.map((e) => `${e.kind}:${e.name}`)), [store]);
	const deps = useMemo(
		() => resolveCatalogClosure(entry, catalog, installedKeys),
		[entry, catalog, installedKeys]
	);
	// Deps that will actually be fetched (not already in the store).
	const missingDeps = deps.filter((d) => !d.satisfied);
	// A non-catalog / un-pinned missing dep needs an explicit extra confirm.
	const needsExtraConfirm = missingDeps.some((d) => d.needsExtraConfirm);

	// Staged install: dep-free primitives install on one click (byte-identical to
	// the prior UX); any missing dep routes through the consent panel first.
	const [stage, setStage] = useState<'idle' | 'consent'>('idle');
	const [extraOk, setExtraOk] = useState(false);

	function onInstallClick() {
		if (missingDeps.length === 0) {
			install.mutate({ entry, catalog });
		} else {
			setExtraOk(false);
			setStage('consent');
		}
	}

	return (
		<>
			<div className="ccfg-detail-head">
				<h2>
					{entry.name}
					<span className="ccfg-scope-pill is-project">{KIND_ABBR[UI_KIND_OF[entry.kind]]}</span>
				</h2>
				<div style={{ marginTop: 6 }}>
					<span className="ngwa-dstate">Ọba catalog · available to install</span>
				</div>
				{entry.description && <div className="ccfg-detail-desc">{entry.description}</div>}
			</div>

			<div className="ccfg-detail-body">
				<Section label="Source">
					<FrontmatterGrid
						entries={[
							['source', entry.source],
							['ref', <span style={{ wordBreak: 'break-all' }}>{entry.url}</span>],
							['version', entry.version],
							...(entry.publisher
								? [['publisher', entry.publisher] as [string, React.ReactNode]]
								: []),
						]}
					/>
				</Section>
				<div className="ccfg-section">
					<div className="ngwa-mech">
						<b>Install resolves to a {entry.source} fetch.</b> Installing clones/fetches the master
						into the Ọba vault and symlinks it into your chosen scope — the same provenance path
						imported primitives use.
					</div>
				</div>
				{deps.length > 0 && (
					<Section label={`Requires (${deps.length})`}>
						<div className="ngwa-guard-deps">
							{deps.map((d) => (
								<ConsentDepLine key={`${d.kind}:${d.name}`} dep={d} />
							))}
						</div>
					</Section>
				)}
			</div>

			<div className="ngwa-actions">
				<div className="ngwa-acts">
					<button
						type="button"
						className="ngwa-btn primary"
						disabled={!isFileBased || install.isPending || stage === 'consent'}
						title={
							isFileBased
								? `Fetch ${entry.name} from its ${entry.source} source into the Ọba vault`
								: `${entry.kind} primitives install via the merge engine, not the vault`
						}
						onClick={onInstallClick}
					>
						{install.isPending ? 'Installing…' : 'Install'}
					</button>
					<span className="ngwa-stword" style={{ textTransform: 'none', opacity: 0.7 }}>
						{install.isError
							? `install failed: ${errText(install.error)}`
							: install.isSuccess
								? install.data && install.data.installed.length > 0
									? `installed with ${install.data.installed.length} dep${install.data.installed.length === 1 ? '' : 's'} (${install.data.installed.map((d) => d.name).join(', ')}) — enable it below`
									: 'installed — enable it in a scope below'
								: stage === 'consent'
									? `also installs ${missingDeps.length} dependenc${missingDeps.length === 1 ? 'y' : 'ies'} — review below`
									: isFileBased
										? missingDeps.length > 0
											? `also installs ${missingDeps.length} dependenc${missingDeps.length === 1 ? 'y' : 'ies'}`
											: `fetches via ${entry.source} into the vault`
										: 'hook/mcp install is not yet supported'}
					</span>
				</div>
			</div>

			{stage === 'consent' && (
				<div className="ngwa-section ngwa-guard">
					<div className="ngwa-guard-head">
						<span className="ngwa-guard-icon">↓</span>
						<span className="ngwa-guard-title">
							Installing <code>{entry.name}</code> also installs {missingDeps.length}{' '}
							{missingDeps.length === 1 ? 'dependency' : 'dependencies'}
						</span>
						<button
							type="button"
							className="ngwa-guard-x"
							onClick={() => setStage('idle')}
							aria-label="Cancel"
						>
							×
						</button>
					</div>
					<div className="ngwa-guard-body">
						<div className="ngwa-guard-line">
							These resolve from <code>{entry.name}</code>'s <code>requires</code> and install under
							the same trust as <code>{entry.name}</code>. Already-installed deps are listed but not
							re-fetched.
						</div>
						<div className="ngwa-guard-deps">
							{deps.map((d) => (
								<ConsentDepLine key={`${d.kind}:${d.name}`} dep={d} />
							))}
						</div>
						{needsExtraConfirm && (
							<label
								className="ngwa-guard-choice"
								style={{ display: 'flex', gap: 8, alignItems: 'flex-start', cursor: 'pointer' }}
							>
								<input
									type="checkbox"
									checked={extraOk}
									onChange={(e) => setExtraOk(e.target.checked)}
									style={{ marginTop: 3 }}
								/>
								<span className="ngwa-guard-line">
									One or more dependencies are <b>not in the signed catalog</b> and will be fetched
									from a self-declared source. I understand and want to install them.
								</span>
							</label>
						)}
						<div className="ngwa-acts" style={{ marginTop: 10 }}>
							<button
								type="button"
								className="ngwa-btn primary"
								disabled={install.isPending || (needsExtraConfirm && !extraOk)}
								onClick={() => install.mutate({ entry, catalog })}
							>
								{install.isPending
									? 'Installing…'
									: `Install ${entry.name} + ${missingDeps.length} dep${missingDeps.length === 1 ? '' : 's'}`}
							</button>
							<button type="button" className="ngwa-btn" onClick={() => setStage('idle')}>
								Cancel
							</button>
						</div>
					</div>
				</div>
			)}
		</>
	);
}

/** One dependency row in the install consent surface (WP-15). */
function ConsentDepLine({ dep }: { dep: ConsentDep }) {
	return (
		<div className="ngwa-guard-dep">
			<span className="ngwa-guard-arrow">↳</span>
			<code>{dep.name}</code>
			<span className="ngwa-mtag">{dep.kind}</span>
			<span className="ngwa-stword" style={{ textTransform: 'none', opacity: 0.7 }}>
				{dep.satisfied ? 'already installed' : `from ${dep.provenance}`}
			</span>
			{!dep.satisfied && dep.resolution !== 'catalog' && (
				<span
					className="ngwa-ovr"
					title={
						dep.resolution === 'unresolved'
							? 'Not in the catalog and no pinned source — may fail to resolve'
							: 'Not in the signed catalog — fetched from a self-declared source'
					}
				>
					{dep.resolution === 'unresolved' ? 'unresolved' : 'non-catalog'}
				</span>
			)}
		</div>
	);
}

function EmptyCarve() {
	return (
		<div className="ccfg-empty">
			<div className="carve">▽▽▽</div>
			Select an entry to inspect its contents,
			<br />
			relationships, and precedence.
		</div>
	);
}

function StateDot({ state }: { state: ItemState }) {
	return <span className={cn('ngwa-dot', state)} title={STATE_WORD[state]} aria-hidden />;
}

function FormatChip({ format }: { format: ConfigFormat | null }) {
	if (!format) return null;
	const f = FORMAT_CHIP[format];
	return <span className={cn('ccfg-fmt', f.cls)}>{f.label}</span>;
}

function BrowseRow({
	item,
	active,
	onClick,
	showEngineMeta = false,
}: {
	item: NgwaItem;
	active: boolean;
	onClick: () => void;
	/** Multi-engine (facet) mode: show the per-row format chip + deprecated chip.
	 *  Off in the Claude-only view so it renders byte-identical to today. */
	showEngineMeta?: boolean;
}) {
	return (
		<button
			type="button"
			onClick={onClick}
			className={cn(
				'ccfg-row text-left w-full',
				active && 'is-on',
				item.overriddenBy && 'is-shadowed'
			)}
		>
			<div className="ccfg-row-name">
				<StateDot state={item.state} />
				<span>{item.name}</span>
				{showEngineMeta && <FormatChip format={item.format} />}
				<span className={cn('ccfg-scope', item.scope === 'personal' && 'is-personal')}>
					{item.scope === 'personal' ? 'pers' : item.scopeLabel}
				</span>
				{item.overriddenBy && (
					<span className="ngwa-ovr" title={`Overridden by a ${item.overriddenBy} entry`}>
						ovr
					</span>
				)}
			</div>
			{item.description && <div className="ccfg-row-desc">{item.description}</div>}
			<div className="ccfg-row-meta">
				{/* G-11: state WORD in every row, not colour-only */}
				<span className={cn('ngwa-stword', item.state)}>{STATE_WORD[item.state]}</span>
				{showEngineMeta && item.status === 'deprecated' && (
					<>
						<span>·</span>
						<span className="ngwa-stword deprecated">deprecated</span>
					</>
				)}
				<span>·</span>
				<span>
					{item.storeEntry ? 'store ↗' : item.scope === 'personal' ? '~/.claude' : '.claude'}
				</span>
				<span className={cn('ngwa-mtag', item.mech)}>{item.mech}</span>
			</div>
		</button>
	);
}

// ─── REGISTRY surface (full-width 8-col table + @filter DSL + bulk) ─────────

interface RegistryProps {
	items: NgwaItem[];
	store: ClaudeStoreEntry[];
	scope: NgwaScopeId;
	activeSystems: ReadonlySet<NgwaSystemId>;
	present: NgwaSystemId[];
	isLoading: boolean;
	error: string | null;
	onEdit: (path: string) => void;
	projectScopes: Array<{ key: ClaudeStoreScope; label: string }>;
}

const FCHIPS = ['@personal', '@project', '@disabled', '@orphaned', '@local'] as const;

function RegistrySurface({
	items,
	scope,
	activeSystems,
	present,
	isLoading,
	error,
	onEdit,
	projectScopes,
}: RegistryProps) {
	const [q, setQ] = useState('');
	const [chips, setChips] = useState<Set<string>>(new Set());
	const [multi, setMulti] = useState<Set<string>>(new Set());
	const [drawerId, setDrawerId] = useState<string | null>(null);

	// Modal dialog focus management for the inspect drawer (WCAG 4.1.2 / 2.4.3).
	// The trap engages while the drawer is open and returns focus to the row that
	// opened it on close. Esc-to-close is wired on the scrim below.
	const modalRef = useRef<HTMLDivElement | null>(null);
	useFocusTrap(modalRef, { enabled: drawerId !== null });

	const enable = useEnablePrimitive();
	const disable = useDisablePrimitive();

	// @filter DSL: chips + inline `@token`s in the query. Free words match
	// name/description; recognised `@token`s narrow scope/state/kind.
	function pass(it: NgwaItem): boolean {
		if (!passScope(it, scope)) return false;
		if (!activeSystems.has(it.system)) return false;
		for (const f of chips) {
			if (!matchToken(it, f.slice(1))) return false;
		}
		for (const t of q.toLowerCase().trim().split(/\s+/).filter(Boolean)) {
			if (t[0] === '@') {
				if (!matchToken(it, t.slice(1))) return false;
			} else if (
				!it.name.toLowerCase().includes(t) &&
				!(it.description ?? '').toLowerCase().includes(t)
			) {
				return false;
			}
		}
		return true;
	}

	const rows = useMemo(
		() => items.filter(pass),
		// eslint-disable-next-line react-hooks/exhaustive-deps
		[items, scope, chips, q, activeSystems]
	);

	const drawerItem = useMemo(
		() => (drawerId ? (items.find((it) => it.id === drawerId) ?? null) : null),
		[drawerId, items]
	);

	const allChecked = rows.length > 0 && rows.every((it) => multi.has(it.id));

	function toggleAll() {
		setMulti(() => (allChecked ? new Set() : new Set(rows.map((it) => it.id))));
	}

	function toggleRowSel(id: string) {
		setMulti((prev) => {
			const next = new Set(prev);
			next.has(id) ? next.delete(id) : next.add(id);
			return next;
		});
	}

	function toggleEnabled(it: NgwaItem) {
		if (it.state === 'enabled') {
			disable.mutate({ kind: it.storeKind, name: it.name, scope: it.scopeKey });
		} else if (it.state === 'disabled') {
			enable.mutate({ kind: it.storeKind, name: it.name, scope: it.scopeKey });
		}
	}

	function bulk(action: 'enable' | 'disable') {
		for (const id of multi) {
			const it = items.find((x) => x.id === id);
			if (!it) continue;
			if (action === 'enable')
				enable.mutate({ kind: it.storeKind, name: it.name, scope: it.scopeKey });
			else disable.mutate({ kind: it.storeKind, name: it.name, scope: it.scopeKey });
		}
		setMulti(new Set());
	}

	return (
		<div className="ngwa-registry">
			<div className="ngwa-toolbar">
				<div className="ccfg-search-wrap">
					<svg
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						strokeWidth={2}
						aria-hidden="true"
					>
						<circle cx="11" cy="11" r="7" />
						<path d="m20 20-3-3" />
					</svg>
					<input
						className="ccfg-search"
						style={{ fontFamily: 'var(--font-mono)' }}
						value={q}
						onChange={(e) => setQ(e.target.value)}
						placeholder="filter…  try @disabled @personal @orphaned @mcp"
						aria-label="Search and filter using @tokens"
					/>
				</div>
				{FCHIPS.map((f) => (
					<button
						key={f}
						type="button"
						className={cn('ngwa-fchip', chips.has(f) && 'on')}
						onClick={() =>
							setChips((prev) => {
								const next = new Set(prev);
								next.has(f) ? next.delete(f) : next.add(f);
								return next;
							})
						}
					>
						{f}
					</button>
				))}
			</div>

			<div className="ngwa-tablewrap">
				<table className="ngwa-table">
					<thead>
						<tr>
							<th style={{ width: 28 }}>
								<input
									type="checkbox"
									checked={allChecked}
									onChange={toggleAll}
									aria-label="Select all"
								/>
							</th>
							<th>Kind</th>
							<th>Name</th>
							<th>Scope</th>
							<th>State</th>
							<th>Source</th>
							<th>Mech</th>
							<th style={{ textAlign: 'right' }}>Actions</th>
						</tr>
					</thead>
					<tbody>
						{isLoading ? (
							<tr>
								<td colSpan={8} style={cellCenter}>
									Loading…
								</td>
							</tr>
						) : error ? (
							<tr>
								<td colSpan={8} style={cellCenter}>
									{error}
								</td>
							</tr>
						) : rows.length === 0 ? (
							<tr>
								<td colSpan={8} style={cellCenter}>
									no matches for this filter
								</td>
							</tr>
						) : (
							rows.map((it) => (
								<tr
									key={it.id}
									className={cn(
										it.state === 'disabled' && 'dis',
										it.overriddenBy && 'is-shadowed',
										drawerId === it.id && 'on'
									)}
									onClick={(e) => {
										if ((e.target as HTMLElement).closest('input,.ngwa-swc,.ngwa-iact')) return;
										setDrawerId(it.id);
									}}
								>
									<td>
										<input
											type="checkbox"
											checked={multi.has(it.id)}
											onChange={() => toggleRowSel(it.id)}
											aria-label={`Select ${it.name}`}
										/>
									</td>
									<td>
										<span className="ngwa-kb">{KIND_ABBR[it.uiKind]}</span>
									</td>
									<td>
										<div className="nm">
											{it.name}
											{it.overriddenBy && <span className="ngwa-ovr">ovr</span>}
										</div>
										{it.description && <div className="desc">{it.description}</div>}
									</td>
									<td>
										<span className={cn('ccfg-scope', it.scope === 'personal' && 'is-personal')}>
											{it.scope === 'personal' ? 'pers' : it.scopeLabel}
										</span>
									</td>
									<td>
										<span className="st">
											<StateDot state={it.state} />
											<span className={cn('ngwa-stword', it.state)}>{STATE_WORD[it.state]}</span>
										</span>
									</td>
									<td>
										<span className="src">
											{it.storeEntry
												? 'store ↗'
												: it.scope === 'personal'
													? '~/.claude'
													: '.claude'}
										</span>
									</td>
									<td>
										<span className={cn('ngwa-mtag', it.mech)}>{it.mech}</span>
									</td>
									<td>
										<div className="ngwa-rowacts">
											{(it.state === 'enabled' || it.state === 'disabled') && (
												<span
													className={cn('ngwa-swc', it.state === 'enabled' && 'on')}
													title="toggle enable"
													role="button"
													tabIndex={0}
													onClick={(e) => {
														e.stopPropagation();
														toggleEnabled(it);
													}}
												/>
											)}
											<button
												type="button"
												className="ngwa-iact"
												onClick={(e) => {
													e.stopPropagation();
													setDrawerId(it.id);
												}}
											>
												inspect
											</button>
										</div>
									</td>
								</tr>
							))
						)}
					</tbody>
				</table>
			</div>

			{multi.size > 0 && (
				<div className="ngwa-bulk">
					<span className="cnt" aria-live="polite" aria-atomic="true">
						<b>{multi.size}</b> selected
					</span>
					<button type="button" className="ngwa-bbtn" onClick={() => bulk('enable')}>
						Enable
					</button>
					<button type="button" className="ngwa-bbtn" onClick={() => bulk('disable')}>
						Disable
					</button>
					<button type="button" className="ngwa-bbtn warn" onClick={() => bulk('disable')}>
						Remove
					</button>
					<button
						type="button"
						className="ngwa-bbtn"
						style={{ marginLeft: 'auto' }}
						onClick={() => setMulti(new Set())}
					>
						Clear
					</button>
				</div>
			)}

			{/* Registry detail opens in a right drawer reusing the superset */}
			{drawerItem && (
				<div
					className="ngwa-scrim"
					onClick={(e) => e.target === e.currentTarget && setDrawerId(null)}
					onKeyDown={(e) => {
						if (e.key === 'Escape') {
							e.stopPropagation();
							setDrawerId(null);
						}
					}}
				>
					<div
						ref={modalRef}
						className="ngwa-modal"
						role="dialog"
						aria-modal="true"
						aria-labelledby="ngwa-reg-modal-title"
						style={{ width: 420, maxHeight: '90vh', overflow: 'auto', padding: 0 }}
						onClick={(e) => e.stopPropagation()}
					>
						<ItemDetail
							item={drawerItem}
							projectScopes={projectScopes}
							onEdit={onEdit}
							siblingSystems={siblingSystemsOf(drawerItem, items)}
							present={present}
							headingId="ngwa-reg-modal-title"
						/>
						<div style={{ padding: 'var(--space-3)', textAlign: 'right' }}>
							<button type="button" className="ngwa-btn" onClick={() => setDrawerId(null)}>
								Close
							</button>
						</div>
					</div>
				</div>
			)}
		</div>
	);
}

const cellCenter: React.CSSProperties = {
	padding: 30,
	textAlign: 'center',
	color: 'var(--fg-faint)',
	fontFamily: 'var(--font-mono)',
	fontSize: 11,
};

function matchToken(it: NgwaItem, w: string): boolean {
	switch (w) {
		case 'personal':
			return it.scope === 'personal';
		case 'project':
			return it.scope === 'project';
		case 'disabled':
			return it.state === 'disabled';
		case 'orphaned':
			return it.state === 'orphaned';
		case 'local':
			return it.state === 'local';
		case 'enabled':
			return it.state === 'enabled';
		default:
			// kind token (skill/agent/cmd/hook/mcp or the ui plural)
			return KIND_ABBR[it.uiKind] === w || it.uiKind.startsWith(w) || it.storeKind === w;
	}
}

// ─── Superset DETAIL (G-01 body · G-03 tree · G-04 precedence · G-07 chips) ─

function asStringArray(v: unknown): string[] {
	if (!Array.isArray(v)) return [];
	return v.filter((x): x is string => typeof x === 'string');
}
function partitionTools(allowed: readonly string[]): { tools: string[]; mcp: string[] } {
	const tools: string[] = [];
	const mcp: string[] = [];
	for (const t of allowed) (t.startsWith('mcp__') ? mcp : tools).push(t);
	return { tools, mcp };
}
function formatBytes(n: number): string {
	if (n < 1024) return `${n} B`;
	if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
	return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

function Section({
	label,
	count,
	children,
}: {
	label: string;
	count?: string;
	children: React.ReactNode;
}) {
	return (
		<div className="ccfg-section">
			<div className="ccfg-section-label">
				<span>{label}</span>
				{count && <span className="ct">{count}</span>}
			</div>
			{children}
		</div>
	);
}

function ItemDetail({
	item,
	projectScopes,
	onEdit,
	siblingSystems = [],
	present = ['claude'],
	headingId,
}: {
	item: NgwaItem;
	projectScopes: Array<{ key: ClaudeStoreScope; label: string }>;
	onEdit: (path: string) => void;
	/** Other engines that have a primitive of the same kind+name (cross-engine
	 *  collision, e.g. the `reviewer` agent under Gemini + Codex). */
	siblingSystems?: NgwaSystemId[];
	/** Engines present in the scan — drives the D-09 cross-engine destination set.
	 *  Defaults to Claude-only so single-engine views behave exactly as before. */
	present?: NgwaSystemId[];
	/** When this detail is the labelling heading of an enclosing dialog, the
	 *  caller passes the id its `aria-labelledby` points at. */
	headingId?: string;
}) {
	const enable = useEnablePrimitive();
	const disable = useDisablePrimitive();
	const move = useMovePrimitive();
	const copy = useCopyPrimitive();
	const remove = useRemovePrimitive();
	const importToStore = useImportToStore();

	const [picker, setPicker] = useState<null | 'move' | 'copy'>(null);
	// D-09: the cross-engine multi-destination transcode drawer (file-based kinds
	// only). For hooks/mcp, "Copy to…" keeps the single-scope same-engine picker.
	const [showCopyDrawer, setShowCopyDrawer] = useState(false);
	const crossEngineCopyable = isCrossEngineCopyable(item.storeKind);
	const [scriptBody, setScriptBody] = useState<string | null>(null);
	// D-01 inline safe-delete guard — open when the user fires Remove against
	// what may be a real master (non-symlink + non-orphaned). For a symlink
	// placement the old per-scope remove is already safe (`remove_file` never
	// recurses), so we keep that fast path.
	const [showGuard, setShowGuard] = useState(false);

	const on = item.state === 'enabled';
	const k = item.storeKind;
	// Skills are DIRECTORY primitives — the importer (`import_core`) needs the
	// skill dir, not the SKILL.md file that `path` points at (a file fails its
	// `is_dir()` check, silently rejecting the import). Single-file kinds
	// (agent/command) keep `path`. Hook/mcp are JSON-merge and never `local`.
	const importSource = item.storeKind === 'skill' ? (item.raw as ClaudeSkill).dirPath : item.path;

	// ── precedence / override callout (G-04) ──
	let prec: React.ReactNode = null;
	if (item.overriddenBy) {
		prec = (
			<div className="ngwa-prec shadowed">
				<b>Shadowed.</b> A <b>{item.overriddenBy}</b> entry of the same name takes precedence — this
				copy is inactive when working under that scope. Orthogonal to enabled/disabled.
				<div className="chain">
					<span className="lose">{item.scopeLabel}</span> ‹{' '}
					<span className="win">{item.overriddenBy} ✓ wins</span>
				</div>
			</div>
		);
	}

	// ── mechanism callout ──
	const mech =
		item.mech === 'merge' ? (
			<div className="ngwa-mech merge">
				<b>JSON-merge.</b> This {k === 'hook' ? 'hook' : 'MCP server'} is a keyed block inside its
				settings file. Enable/disable splices only its key — every unrelated key (incl.
				session/OAuth state) is preserved. No symlink.
			</div>
		) : (
			<div className="ngwa-mech">
				<b>Symlink → store.</b> Enabling links{' '}
				<code>
					.claude/{k}s/{item.name}
				</code>{' '}
				into the central store (Ọba). Disabling drops the link; the store copy stays.
			</div>
		);

	// ── kind-specific frontmatter + chips + body ──
	const { fmRows, chips, body, tree } = detailParts(item);

	function loadScript() {
		const h = item.raw as ClaudeHook;
		if (!h.commandPath) return;
		claudeConfigReadFile(h.commandPath)
			.then(setScriptBody)
			.catch((e) => setScriptBody(String(e)));
	}

	return (
		<>
			<div className="ccfg-detail-head">
				<div className="ccfg-detail-topline">
					<button
						type="button"
						className="ccfg-filepath"
						title={`Reveal ${item.path}`}
						onClick={() => onEdit(item.path)}
					>
						{shortPath(item.path)}
					</button>
				</div>
				<h2 id={headingId}>
					<span className={cn('ccfg-eb', ENGINE_META[item.system].code)} aria-hidden>
						<EngineGlyph system={item.system} />
					</span>
					{item.name}
					<StateDot state={item.state} />
					<span
						className={cn(
							'ccfg-scope-pill',
							item.scope === 'project' ? 'is-project' : 'is-personal'
						)}
					>
						{item.scope}
					</span>
				</h2>
				<div style={{ display: 'flex', gap: 8, alignItems: 'center', marginTop: 6 }}>
					<span className="ngwa-dstate">
						<StateDot state={item.state} /> {STATE_WORD[item.state]}
					</span>
					<span className={cn('ngwa-mtag', item.mech)}>{item.mech}</span>
					{item.format && <FormatChip format={item.format} />}
					{item.status === 'deprecated' && (
						<span className="ngwa-stword deprecated">deprecated</span>
					)}
				</div>
				{item.description && <div className="ccfg-detail-desc">{item.description}</div>}
			</div>

			<div className="ccfg-detail-body">
				{prec && <div className="ccfg-section">{prec}</div>}

				{siblingSystems.length > 0 && (
					<Section label="Same name across systems">
						<div className="ngwa-mech">
							A <b>{k}</b> named <b>{item.name}</b> also exists under{' '}
							{siblingSystems.map((s, i) => (
								<span key={s}>
									{i > 0 && ', '}
									<span className={cn('ccfg-eb', ENGINE_META[s].code)} aria-hidden>
										<EngineGlyph system={s} />
									</span>{' '}
									{ENGINE_META[s].display}
								</span>
							))}
							. Ngwa keys identity on <code>{'{system, kind, scope, name}'}</code>, so these are
							distinct primitives shown side by side.
						</div>
					</Section>
				)}

				<Section label="Identity">
					<FrontmatterGrid entries={fmRows} />
				</Section>

				{/* Provenance (Ọba registry · G-SCHEMA) — where this primitive's
				    canonical master came from. Two sources:
				    (1) the registry record (storeEntry.source) — preferred; carries
				        version/url/managed/installedAt for git/npx installs;
				    (2) scanner-derived fallback for a `linked` state — a symlink
				        resolving to an external master that the registry doesn't
				        yet index (the common pre-back-fill case, e.g. groundwork
				        published from ikenga-pkgs into ~/.claude/skills/). The
				        master path comes from the live link resolution. Dependents
				        + the safe-delete guard are the interactive WP-06 slice. */}
				{item.storeEntry?.source ? (
					<Section label="Provenance">
						<FrontmatterGrid
							entries={[
								['source', item.storeEntry.source],
								...(item.storeEntry.url ? [['url', item.storeEntry.url] as [string, string]] : []),
								...(item.storeEntry.version
									? [['version', item.storeEntry.version] as [string, string]]
									: []),
								[
									'master',
									item.storeEntry.managed === false
										? 'external · kept in place'
										: 'vault · shell-managed',
								],
								...(item.storeEntry.canonicalPath
									? [['canonical', item.storeEntry.canonicalPath] as [string, string]]
									: []),
							]}
						/>
					</Section>
				) : item.state === 'linked' ? (
					<Section label="Provenance">
						<FrontmatterGrid
							entries={[
								['source', 'external symlink'],
								[
									'canonical',
									(item.raw as { linkTarget?: string | null }).linkTarget ?? '(unresolved)',
								],
								['master', 'external · kept in place'],
							]}
						/>
					</Section>
				) : null}

				{chips}

				<div className="ccfg-section">{mech}</div>

				{item.state === 'local' && (
					<div className="ccfg-section">
						<div className="ngwa-mech merge">
							<b>Not in store.</b> A real file in this scope. Import it to enable symlink-based
							reuse across projects.
						</div>
					</div>
				)}
				{item.state === 'orphaned' && (
					<div className="ccfg-section">
						<div className="ngwa-mech warn">
							<b>Orphaned.</b> The symlink target was removed from the store. Remove the dangling
							link, or re-link to a store entry.
						</div>
					</div>
				)}
				{item.state === 'linked' && (
					<div className="ccfg-section">
						<div className="ngwa-mech">
							<b>Linked.</b> A symlink resolving to a valid master outside the store
							{item.storeEntry?.managed === false ? ' (kept in place — externally mastered)' : ''}.
							Reads as healthy, not orphaned — updating the master updates this link in place.
						</div>
					</div>
				)}

				{/* G-01 / G-05: body or hook script */}
				{k === 'hook' ? (
					<Section
						label="Script"
						count={scriptBody ? `${scriptBody.split('\n').length} lines` : 'load script'}
					>
						{(item.raw as ClaudeHook).commandPath && !scriptBody && (
							<button type="button" className="ngwa-btn" onClick={loadScript}>
								Load script
							</button>
						)}
						<pre className="ccfg-script">
							{scriptBody ?? JSON.stringify((item.raw as ClaudeHook).raw, null, 2)}
						</pre>
					</Section>
				) : k === 'mcp' ? (
					<Section label="Server config">
						<pre className="ccfg-script">
							{JSON.stringify((item.raw as ClaudeMcp).raw, null, 2)}
						</pre>
					</Section>
				) : (
					<Section
						label={k === 'agent' ? 'System prompt · body' : 'Body'}
						count={`${body.split('\n').length} lines`}
					>
						<div className="ccfg-body-md">
							<Markdown content={body} density="compact" />
						</div>
					</Section>
				)}

				{/* G-03 supporting-files tree (skills) */}
				{tree}
			</div>

			{/* write actions (G-10) — pinned footer */}
			<div className="ngwa-actions">
				<div className="ngwa-acts">
					{(item.state === 'enabled' || item.state === 'disabled') && (
						<label className="ngwa-toggle">
							<span
								className={cn('ngwa-sw', on && 'on')}
								role="button"
								tabIndex={0}
								onClick={() =>
									on
										? disable.mutate({ kind: k, name: item.name, scope: item.scopeKey })
										: enable.mutate({ kind: k, name: item.name, scope: item.scopeKey })
								}
							/>
							{item.mech === 'merge' ? (on ? 'Merged in' : 'Removed') : on ? 'Enabled' : 'Disabled'}
						</label>
					)}
					{item.state === 'local' && (
						<button
							type="button"
							className="ngwa-btn primary"
							disabled={importToStore.isPending}
							onClick={() =>
								importToStore.mutate({ kind: k, name: item.name, sourcePath: importSource })
							}
						>
							Import to store
						</button>
					)}
					{item.state === 'orphaned' && (
						<button
							type="button"
							className="ngwa-btn warn"
							onClick={() => remove.mutate({ kind: k, name: item.name, scope: item.scopeKey })}
						>
							Remove dangling link
						</button>
					)}
					<button type="button" className="ngwa-btn" onClick={() => setPicker('move')}>
						Move to…
					</button>
					<button
						type="button"
						className="ngwa-btn"
						onClick={() => (crossEngineCopyable ? setShowCopyDrawer(true) : setPicker('copy'))}
						title={
							crossEngineCopyable
								? 'Copy to one or more engine + scope destinations (transcode where needed)'
								: undefined
						}
					>
						Copy to…
					</button>
					{item.state !== 'orphaned' && (
						<button
							type="button"
							className="ngwa-btn warn"
							onClick={() => {
								// Symlink placement → per-scope unlink is always safe (`remove_file`
								// never recurses). Real master / unknown → route through the
								// dependent-aware safe-delete guard (D-01, WP-04 backend).
								const isPlacementSymlink = (item.raw as { isSymlink?: boolean }).isSymlink;
								if (isPlacementSymlink) {
									remove.mutate({ kind: k, name: item.name, scope: item.scopeKey });
								} else {
									setShowGuard(true);
								}
							}}
						>
							{item.storeEntry ? 'Remove from scope' : 'Delete from store'}
						</button>
					)}
					<button type="button" className="ngwa-btn" onClick={() => onEdit(item.path)}>
						{item.mech === 'merge' ? 'Open settings' : 'Reveal'}
					</button>
				</div>
			</div>

			{picker && (
				<ScopePicker
					title={picker === 'move' ? 'Move to…' : 'Copy to…'}
					desc={`Destination scope for "${item.name}".`}
					scopes={projectScopes}
					disableScope={item.scopeKey}
					onPick={(toScope) => {
						if (picker === 'move')
							move.mutate({ kind: k, name: item.name, fromScope: item.scopeKey, toScope });
						else copy.mutate({ kind: k, name: item.name, fromScope: item.scopeKey, toScope });
						setPicker(null);
					}}
					onClose={() => setPicker(null)}
				/>
			)}

			{showGuard && (
				<SafeDeleteGuard
					kind={k}
					name={item.name}
					selfPath={item.path}
					onClose={() => setShowGuard(false)}
				/>
			)}

			{showCopyDrawer && (
				<WriteTranscodeDrawer
					item={item}
					present={present}
					projectScopes={projectScopes}
					onClose={() => setShowCopyDrawer(false)}
				/>
			)}
		</>
	);
}

function detailParts(item: NgwaItem): {
	fmRows: Array<[string, React.ReactNode]>;
	chips: React.ReactNode;
	body: string;
	tree: React.ReactNode;
} {
	const fmRows: Array<[string, React.ReactNode]> = [
		['system', ENGINE_META[item.system].display],
		['kind', item.storeKind],
		...(item.format
			? ([['format', FORMAT_LABEL[item.format]]] as Array<[string, React.ReactNode]>)
			: []),
		[
			'scope',
			<span>
				{item.scopeLabel}
				{item.projectRoot && (
					<span style={{ color: 'var(--fg-faint)' }}> · {shortPath(item.projectRoot)}</span>
				)}
			</span>,
		],
		[
			'source',
			item.storeEntry ? 'store ↗ (canonical)' : item.scope === 'personal' ? '~/.claude' : '.claude',
		],
	];

	let chips: React.ReactNode = null;
	let body = '';
	let tree: React.ReactNode = null;

	if (item.storeKind === 'agent') {
		const a = item.raw as ClaudeAgent;
		body = a.body;
		if (a.model) fmRows.push(['model', a.model]);
		const allowed = asStringArray(a.frontmatter['allowed-tools']);
		const skillsUsed = asStringArray(a.frontmatter['skills-used']);
		const { tools, mcp } = partitionTools(allowed);
		chips = (
			<>
				{skillsUsed.length > 0 && (
					<Section label="Skills used" count={`${skillsUsed.length}`}>
						<Chips values={skillsUsed} variant="skill" />
					</Section>
				)}
				{tools.length > 0 && (
					<Section label="Allowed tools" count={`${tools.length}`}>
						<Chips values={tools} variant="tool" initial={tools.length > 8 ? 8 : undefined} />
					</Section>
				)}
				{mcp.length > 0 && (
					<Section label="MCP tools" count={`${mcp.length}`}>
						<Chips values={mcp} variant="mcp" />
					</Section>
				)}
			</>
		);
	} else if (item.storeKind === 'skill') {
		const s = item.raw as ClaudeSkill;
		body = s.body;
		const allowed = asStringArray(s.frontmatter['allowed-tools']);
		chips =
			allowed.length > 0 ? (
				<Section label="Allowed tools" count={`${allowed.length}`}>
					<Chips values={allowed} variant="tool" />
				</Section>
			) : null;
		if (s.supportingFiles.length > 0) {
			tree = (
				<Section
					label="Supporting files"
					count={`${s.supportingFiles.length} files · ${formatBytes(
						s.supportingFiles.reduce((sum, f) => sum + f.size, 0)
					)}`}
				>
					<div className="ccfg-tree">
						<div className="ccfg-tree-row dir">{s.dirPath.split('/').filter(Boolean).pop()}/</div>
						<div className="ccfg-tree-row">
							<span className="indent" /> SKILL.md
						</div>
						{s.supportingFiles.map((f) => (
							<div className="ccfg-tree-row" key={f.path}>
								<span className="indent" /> {f.name}
								<span className="size">{formatBytes(f.size)}</span>
							</div>
						))}
					</div>
				</Section>
			);
		}
	} else if (item.storeKind === 'command') {
		const c = item.raw as ClaudeCommand;
		body = c.body;
		if (c.model) fmRows.push(['model', c.model]);
		if (c.argumentHint) fmRows.push(['argument-hint', c.argumentHint]);
		const allowed = asStringArray(c.frontmatter['allowed-tools']);
		const { tools, mcp } = partitionTools(allowed);
		chips = (
			<>
				{tools.length > 0 && (
					<Section label="Allowed tools" count={`${tools.length}`}>
						<Chips values={tools} variant="tool" initial={tools.length > 8 ? 8 : undefined} />
					</Section>
				)}
				{mcp.length > 0 && (
					<Section label="MCP tools" count={`${mcp.length}`}>
						<Chips values={mcp} variant="mcp" />
					</Section>
				)}
			</>
		);
	} else if (item.storeKind === 'hook') {
		const h = item.raw as ClaudeHook;
		fmRows.push(['event', h.event]);
		fmRows.push(['type', h.type]);
		chips = (
			<Section label="Event">
				<Chips values={[h.event]} variant="event" />
			</Section>
		);
	} else if (item.storeKind === 'mcp') {
		const m = item.raw as ClaudeMcp;
		fmRows.push(['transport', m.transport]);
		if (m.command) fmRows.push(['command', m.command]);
		if (m.url) fmRows.push(['url', m.url]);
		chips =
			m.envKeys.length > 0 ? (
				<Section label="Env vars" count={`${m.envKeys.length}`}>
					<Chips values={m.envKeys} variant="tool" />
				</Section>
			) : null;
	}

	return { fmRows, chips, body, tree };
}

// ─── STORE / catalog detail (G-09: installed-in + install-into-scope) ───────

function StoreDetail({
	entry,
	projectScopes,
	onEdit,
	updatable = false,
	catalogVersion = null,
}: {
	entry: ClaudeStoreEntry;
	projectScopes: Array<{ key: ClaudeStoreScope; label: string }>;
	onEdit: (path: string) => void;
	/** True when the catalog lists a newer version than the installed one. */
	updatable?: boolean;
	/** The catalog's version, shown in the Update affordance. */
	catalogVersion?: string | null;
}) {
	const enable = useEnablePrimitive();
	const disable = useDisablePrimitive();
	const update = useObaUpdate();
	const setAutoUpdate = useObaSetAutoUpdate();
	const [installing, setInstalling] = useState(false);

	const enabledSet = new Set(entry.enabledIn);
	// Phase 3 — only a vault-managed master with a remote (git/npx/catalog) can be
	// auto-updated; a local copy or external master has nothing to re-fetch.
	const canAutoUpdate = entry.managed !== false && entry.source != null && entry.source !== 'local';
	const autoOn = entry.autoUpdate === true;

	return (
		<>
			<div className="ccfg-detail-head">
				<div className="ccfg-detail-topline">
					<button
						type="button"
						className="ccfg-filepath"
						title={`Reveal ${entry.storePath}`}
						onClick={() => onEdit(entry.storePath)}
					>
						{shortPath(entry.storePath)}
					</button>
				</div>
				<h2>
					{entry.name}
					<span className="ccfg-scope-pill is-project">{KIND_ABBR[UI_KIND_OF[entry.kind]]}</span>
				</h2>
				<div style={{ marginTop: 6 }}>
					<span className="ngwa-dstate">Ọba · canonical store copy</span>
				</div>
				{entry.description && <div className="ccfg-detail-desc">{entry.description}</div>}
			</div>

			<div className="ccfg-detail-body">
				<Section label="Identity">
					<FrontmatterGrid
						entries={[
							['kind', entry.kind],
							[
								'store path',
								<span style={{ wordBreak: 'break-all' }}>{shortPath(entry.storePath)}</span>,
							],
						]}
					/>
				</Section>

				{/* WP-10c: update available — re-fetch the managed canonical in place */}
				{updatable && (
					<Section label="Update available">
						<div className="ngwa-mech" style={{ marginBottom: 8 }}>
							Catalog has <b>{catalogVersion ?? 'a newer version'}</b>. Updating re-fetches the
							canonical in place — existing scope symlinks resolve to the refreshed files (no
							relink).
						</div>
						<div className="ngwa-acts">
							<button
								type="button"
								className="ngwa-btn primary"
								disabled={update.isPending}
								onClick={() => update.mutate({ kind: entry.kind, name: entry.name })}
							>
								{update.isPending ? 'Updating…' : `Update → ${catalogVersion ?? 'latest'}`}
							</button>
							{update.isError && (
								<span className="ngwa-stword" style={{ textTransform: 'none', opacity: 0.7 }}>
									update failed: {errText(update.error)}
								</span>
							)}
						</div>
					</Section>
				)}

				{/* Phase 3 — per-entry auto-update opt-in (catalog ON by default, manual
				    OFF). Only meaningful for a managed master with a remote. */}
				{canAutoUpdate && (
					<Section label="Auto-update">
						<label className="ngwa-toggle">
							<span
								className={cn('ngwa-sw', autoOn && 'on')}
								role="button"
								tabIndex={0}
								aria-pressed={autoOn}
								onClick={() =>
									setAutoUpdate.mutate({
										kind: entry.kind,
										name: entry.name,
										enabled: !autoOn,
									})
								}
							/>
							<span className="ngwa-stword" style={{ textTransform: 'none' }}>
								{autoOn
									? 'On — refreshed automatically on the catalog surface'
									: 'Off — update manually'}
							</span>
						</label>
						<div className="ngwa-mech" style={{ marginTop: 6, opacity: 0.7 }}>
							{entry.fromCatalog
								? 'Installed from the Ọba catalog.'
								: `Installed directly (${entry.source}).`}
							{setAutoUpdate.isError && ` · failed: ${errText(setAutoUpdate.error)}`}
						</div>
					</Section>
				)}

				{/* G-09: installed-in scopes */}
				<Section label="Installed in" count={`${entry.enabledIn.length} scopes`}>
					<div className="ngwa-scopechips">
						{entry.enabledIn.length === 0 ? (
							<span className="ngwa-scopechip empty">Not installed in any scope</span>
						) : (
							entry.enabledIn.map((s) => (
								<span key={s} className="ngwa-scopechip">
									{s === 'workspace' ? 'workspace' : s.replace('project:', '')}
								</span>
							))
						)}
					</div>
				</Section>

				{/* G-09: install-into-scope picker */}
				<Section label="Install into scope">
					<div className="ngwa-acts">
						<button type="button" className="ngwa-btn primary" onClick={() => setInstalling(true)}>
							Install into scope…
						</button>
					</div>
				</Section>

				{/* per-scope enable/disable toggles */}
				<Section label="Per-scope state">
					<div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
						{[
							{ key: 'workspace' as ClaudeStoreScope, label: 'Personal / workspace' },
							...projectScopes,
						].map((s) => {
							const isOn = enabledSet.has(s.key);
							return (
								<label key={s.key} className="ngwa-toggle">
									<span
										className={cn('ngwa-sw', isOn && 'on')}
										role="button"
										tabIndex={0}
										onClick={() =>
											isOn
												? disable.mutate({ kind: entry.kind, name: entry.name, scope: s.key })
												: enable.mutate({ kind: entry.kind, name: entry.name, scope: s.key })
										}
									/>
									<span className="ngwa-stword" style={{ textTransform: 'none' }}>
										{s.label}
									</span>
								</label>
							);
						})}
					</div>
				</Section>
			</div>

			{installing && (
				<ScopePicker
					title="Install into scope…"
					desc={`Symlink/merge "${entry.name}" into a scope.`}
					scopes={[{ key: 'workspace', label: 'Personal / workspace' }, ...projectScopes]}
					disabledScopes={entry.enabledIn}
					onPick={(scope) => {
						enable.mutate({ kind: entry.kind, name: entry.name, scope });
						setInstalling(false);
					}}
					onClose={() => setInstalling(false)}
				/>
			)}
		</>
	);
}

// ─── Safe-delete guard (D-01, WP-06) ────────────────────────────────────────
// Inline panel that fires when the user clicks Remove on what may be a real
// canonical master. Calls `oba_safe_delete` (the dependent-aware backend guard
// from WP-04): a symlink placement unlinks straight through; a real master with
// `managed:false` OR live dependents refuses and surfaces the dependents list
// + a relink form. NEVER reaches `remove_dir_all` on a master with dependents
// — the WP-04 backend test `incident_regression_external_master_with_dependents_is_never_wiped`
// proves this. Renders inline in the detail-pane footer (Variant A placement
// of the D-01 hybrid; the SVG dependency map from Variant B is the next polish).

function SafeDeleteGuard({
	kind,
	name,
	selfPath,
	onClose,
}: {
	kind: ClaudeStoreKind;
	name: string;
	selfPath: string;
	onClose: () => void;
}) {
	const relink = useRelinkDependents();
	// Live dependents scan — used both as the proactive display before the user
	// commits, and (after) to confirm relink succeeded by re-checking.
	const depsQ = useQuery(obaDependentsQueryOptions(kind, name));

	// Fire obaSafeDelete on mount with a plain useState — the useMutation
	// hook had a reproducible bug here where its `data` never reflected the
	// resolved invoke result (the wrapper's console.log showed start+resolved
	// in ~40ms, but useMutation stayed in status=pending forever). The plain
	// pattern works fine since we don't need any of the extras useMutation
	// provides for this one-shot, mount-time call.
	const [outcome, setOutcome] = useState<SafeDeleteOutcome | null>(null);
	const [safeErr, setSafeErr] = useState<string | null>(null);
	const fired = useRef(false);
	useEffect(() => {
		if (fired.current) return;
		fired.current = true;
		obaSafeDelete(kind, name)
			.then((r) => setOutcome(r))
			.catch((e) => setSafeErr(String(e)));
	}, [kind, name]);

	const [newMaster, setNewMaster] = useState('');
	const refused =
		outcome?.verdict === 'refused_external' || outcome?.verdict === 'refused_dependents';
	const dependents = outcome?.dependents ?? depsQ.data ?? [];

	return (
		<div className="ngwa-section ngwa-guard">
			<div className="ngwa-guard-head">
				<span className="ngwa-guard-icon">!</span>
				<span className="ngwa-guard-title">
					{!outcome ? (
						'Checking dependents…'
					) : refused ? (
						<>
							<b>Delete refused</b> ·{' '}
							{outcome.verdict === 'refused_external'
								? 'external master, kept in place'
								: `${dependents.length} live dependent${dependents.length === 1 ? '' : 's'}`}
						</>
					) : (
						<>
							<b>{outcome.verdict === 'unlinked' ? 'Unlinked' : 'Deleted'}.</b> {outcome.message}
						</>
					)}
				</span>
				<button type="button" className="ngwa-guard-x" onClick={onClose} aria-label="Close">
					✕
				</button>
			</div>

			{outcome && refused && (
				<div className="ngwa-guard-body">
					<div className="ngwa-guard-line">
						{outcome.message}. Pick a safe path below — the guard will not run{' '}
						<code>remove_dir_all</code> on the master regardless.
					</div>
					{selfPath && (
						<div className="ngwa-guard-self">
							<span className="ngwa-guard-k">master</span> <code>{selfPath}</code>
						</div>
					)}
					<div className="ngwa-guard-deps">
						{dependents.length === 0 ? (
							<div className="ngwa-guard-line">No live dependents found.</div>
						) : (
							dependents.map((p) => (
								<div key={p} className="ngwa-guard-dep">
									<span className="ngwa-guard-arrow">↳</span>
									<code>{p}</code>
								</div>
							))
						)}
					</div>

					{outcome.verdict === 'refused_dependents' && dependents.length > 0 && (
						<div className="ngwa-guard-choice">
							<div className="ngwa-guard-k">Relink dependents to a new master</div>
							<div className="ngwa-guard-relink">
								<input
									type="text"
									className="ngwa-guard-input"
									placeholder="absolute path to the new master…"
									value={newMaster}
									onChange={(e) => setNewMaster(e.target.value)}
								/>
								<button
									type="button"
									className="ngwa-btn primary"
									disabled={!newMaster || relink.isPending}
									onClick={() =>
										relink.mutate(
											{ dependents, newMaster },
											{
												onSuccess: () => {
													// Re-query dependents; the user can then retry the
													// delete if everything relinked.
													depsQ.refetch();
												},
											}
										)
									}
								>
									{relink.isPending ? 'Relinking…' : `Relink ${dependents.length}`}
								</button>
							</div>
							{relink.data && (
								<div className="ngwa-guard-relinkresult">
									{relink.data.filter((r) => r.ok).length} relinked OK ·{' '}
									{relink.data.filter((r) => !r.ok).length} failed
									{relink.data.some((r) => !r.ok) && (
										<ul>
											{relink.data
												.filter((r) => !r.ok)
												.map((r) => (
													<li key={r.link}>
														<code>{r.link}</code>: {r.error}
													</li>
												))}
										</ul>
									)}
								</div>
							)}
						</div>
					)}

					{outcome.verdict === 'refused_external' && (
						<div className="ngwa-guard-line">
							<b>External masters are never deleted by Ngwa.</b> The master lives upstream — manage
							it where it's published (git repo / npx install).
						</div>
					)}
				</div>
			)}

			{outcome && !refused && (
				<div className="ngwa-guard-body">
					<div className="ngwa-guard-line">{outcome.message}</div>
					<button type="button" className="ngwa-btn" onClick={onClose}>
						Close
					</button>
				</div>
			)}

			{safeErr && (
				<div className="ngwa-guard-body">
					<div className="ngwa-guard-line ngwa-guard-err">
						<b>oba_safe_delete failed.</b> {safeErr}
					</div>
				</div>
			)}
		</div>
	);
}

// ─── Scope picker modal ─────────────────────────────────────────────────────

function ScopePicker({
	title,
	desc,
	scopes,
	disableScope,
	disabledScopes,
	onPick,
	onClose,
}: {
	title: string;
	desc: string;
	scopes: Array<{ key: ClaudeStoreScope; label: string }>;
	disableScope?: ClaudeStoreScope;
	disabledScopes?: ClaudeStoreScope[];
	onPick: (scope: ClaudeStoreScope) => void;
	onClose: () => void;
}) {
	const dis = new Set(disabledScopes ?? []);
	// Modal dialog focus management (WCAG 4.1.2 / 2.4.3) — trap + focus-return.
	const pickerRef = useRef<HTMLDivElement | null>(null);
	useFocusTrap(pickerRef, { enabled: true });
	return (
		<div
			className="ngwa-scrim"
			onClick={(e) => e.target === e.currentTarget && onClose()}
			onKeyDown={(e) => {
				if (e.key === 'Escape') {
					e.stopPropagation();
					onClose();
				}
			}}
		>
			<div
				ref={pickerRef}
				className="ngwa-modal"
				role="dialog"
				aria-modal="true"
				aria-labelledby="ngwa-scope-picker-title"
			>
				<h3 id="ngwa-scope-picker-title">{title}</h3>
				<p>{desc}</p>
				<div className="ngwa-pick">
					{scopes.map((s) => {
						const disabled = s.key === disableScope || dis.has(s.key);
						return (
							<button
								key={s.key}
								type="button"
								disabled={disabled}
								onClick={() => !disabled && onPick(s.key)}
							>
								{s.label}
								<span className="pp">
									{s.key === 'workspace' ? '~/.claude' : '.claude/'}
									{disabled ? ' · already there' : ''}
								</span>
							</button>
						);
					})}
				</div>
				<div className="foot">
					<button type="button" className="ngwa-btn" onClick={onClose}>
						Cancel
					</button>
				</div>
			</div>
		</div>
	);
}
