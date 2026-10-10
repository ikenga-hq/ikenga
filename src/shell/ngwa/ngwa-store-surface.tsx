// Ngwa Store Surface (WP-15 / locked D-02).
//
// Registry catalog browsing, package installation, and updates:
// - Updates strip: "Update all (N)" opens a review of each pending update; an
//   update held back for new permissions is "needs approval · Review" (the
//   trust review modal), never "failed"
// - Kind & Trust facet chips over the whole (locally filtered) index
// - Rows carry the closure + asks chips, read from the pkg's detail file
// - Install sheet: overview, the `requires` closure, per-permission consent
//   ("Share kola"), trust, settings, and a sticky foot whose Install button
//   stays disabled until every consent is ticked
//
// The registry index only lists name / latest / kind / description, so the
// closure and permissions come from the per-pkg detail file, fetched lazily
// for the selected row. Rows that haven't been read say so ("permissions not
// read") rather than guessing.

import { useMemo, useRef, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
	AlertTriangle,
	Check,
	Download,
	Link2,
	Search,
	Shield,
	ShieldAlert,
	X,
} from 'lucide-react';
import { ErrorState, LoadingState, OfflineState } from '@/components/states';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import type { NgwaCatalogRow, NgwaStoreEntry, StoreSource } from '@/lib/ngwa/enrichment';
import {
	isNeedsApproval,
	pkgProjectTarget,
	type PrimitiveInstallOutcome,
	type PrimitiveInstallStage,
} from '@/lib/ngwa/use-store-install';
import type { UpdateApprovals } from '@/lib/ngwa/use-update-approvals';
import {
	catalogPin,
	resolveCatalogClosure,
	shortSha,
	type ConsentDep,
	type PrimitiveCatalogEntry,
} from '@/lib/registry/primitives';
import type { ClaudeStoreEntry, ClaudeStoreKind, ResolvedSource } from '@/lib/tauri-cmd';
import {
	AddUrlSheet,
	CatalogSheet,
	CatalogStoreRow,
	InstallSplit,
	consentBlockedReason,
} from './ngwa-store-primitives';
import {
	asksLabel,
	closureLabel,
	consentGroups,
	formatBytes,
	type StoreManifest,
} from '@/lib/ngwa/store-detail';
import type { StorePkgVersion } from '@/lib/registry/client';
import { registryKeys } from '@/lib/registry/use-registry';
import type { NgwaItem } from '@ikenga/contract';
import { kindIcon } from './ngwa-list';
import { NgwaTrustSheet } from './ngwa-trust-sheet';
import { NOT_AVAILABLE_ON_SERVER_YET } from '@/lib/transport/unavailable';
import './ngwa.css';

import type { StoreInstallScope } from '@/lib/ngwa/use-store-install';
import { cancelStoreInstall } from '@/lib/ngwa/use-store-install';
import { classifyInstallError } from '@/lib/ngwa/install-errors';
import { useInstallRun, type InstallRun } from '@/lib/ngwa/install-progress';
import { InstallProgressRow } from './install-progress-row';

export type { StoreInstallScope };

/**
 * Lazily reads one pkg's registry detail file (`pkgs/<short>.json`) and
 * returns the entry for `entry.latestVersion` (manifest, size, integrity).
 */
export type StoreDetailLoader = (
	entry: NgwaStoreEntry,
	signal?: AbortSignal
) => Promise<StorePkgVersion>;

export interface NgwaStoreSurfaceProps {
	catalog: NgwaStoreEntry[];
	/** Reason a registry PACKAGE's install/update is disabled on this host (e.g.
	 *  "Packages are installed by the server operator"). Also the fallback for
	 *  the primitive controls when {@link primitiveDisabledReason} is not given. */
	disabledReason?: string;
	/** Reason an Ọba PRIMITIVE's (catalog row / pasted URL) install/update is
	 *  disabled, when it differs from {@link disabledReason}. */
	primitiveDisabledReason?: string;
	/** A reason a pasted install source cannot be installed on this host (a
	 *  local path in a browser session: "Local installs are desktop-only"), or
	 *  `null`. Shown in the Add-from-URL sheet instead of letting Resolve fail. */
	sourceBlock?: (raw: string) => string | null;
	isLoading?: boolean;
	/** The Ngwa snapshot failed: what is installed is unknown, so no row can
	 *  say installed / update / install. Not a registry outage. */
	error?: Error | null;
	/** Re-read the snapshot (the snapshot error state's one next action). */
	onRetry?: () => void;
	/**
	 * Detail-file loader for the selected row. Absent (e.g. the index hasn't
	 * loaded) → the sheet can't read the closure or permissions, so Install
	 * stays disabled.
	 */
	loadDetail?: StoreDetailLoader;
	/** Display name of the active project — the default install target. */
	activeProjectName?: string;
	/** Id of the active project. When it is the Default project a pkg has no
	 *  project target: it installs to personal (DEC-71). */
	activeProjectId?: string | null;
	/** Install to a scope. A returned promise drives the foot's "Registering"
	 *  state; a rejection is shown in the sheet foot. */
	onInstall?: (entry: NgwaStoreEntry, scope: StoreInstallScope) => void | Promise<unknown>;
	/** Update one installed pkg to `latestVersion`; same promise contract. */
	onUpdate?: (entry: NgwaStoreEntry) => void | Promise<unknown>;
	/** Update every pending entry after the review dialog is confirmed. */
	onUpdateAll?: (entries: NgwaStoreEntry[]) => void | Promise<unknown>;
	/**
	 * Where an update held back for new permissions goes (`NeedsApprovalError`
	 * from `onUpdate` / `onUpdateAll`): the route's `useUpdateApprovals()`,
	 * whose modal it mounts. Absent → the hold reads as a failure.
	 */
	updateApprovals?: Pick<UpdateApprovals, 'pending' | 'request' | 'review'>;

	// ── R57 · git / npx primitives ──
	/** Signed-catalog rows, listed after the registry pkgs (Q2 duplicates
	 *  already folded into their registry row). */
	primitives?: NgwaCatalogRow[];
	/** The whole signed catalog (closures resolve against it). */
	catalogEntries?: PrimitiveCatalogEntry[];
	/** The vault as listed now (the closure's "already installed"). */
	vault?: ClaudeStoreEntry[];
	/** `verified` = the remote, minisign-verified; `seed` = the bundled copy
	 *  (remote absent); `error` = the remote failed to verify — nothing from
	 *  it is listed, and the seed is never substituted. */
	catalogStatus?: 'loading' | 'verified' | 'seed' | 'error';
	catalogError?: string | null;
	onRecheckCatalog?: () => void;
	onInstallPrimitive?: (
		row: NgwaCatalogRow,
		scope: StoreInstallScope,
		onStage: (s: PrimitiveInstallStage) => void
	) => Promise<PrimitiveInstallOutcome>;
	/** Q4: move a catalog install to its catalog pin. */
	onUpdatePrimitive?: (row: NgwaCatalogRow) => Promise<unknown>;
	/** Add from URL: the dry-run resolve (N-B). */
	onResolveSource?: (
		url: string,
		opts: { kind: ClaudeStoreKind | null; name: string | null; gitRef: string | null }
	) => Promise<ResolvedSource>;
	/** Add from URL: install what was resolved, pinned, then place it. */
	onInstallResolved?: (
		resolved: ResolvedSource,
		scope: StoreInstallScope,
		onStage: (s: PrimitiveInstallStage) => void
	) => Promise<PrimitiveInstallOutcome>;
	/** Done state's "Open in Installed". */
	onOpenInstalled?: (name: string) => void;
	/** Open Add from URL on mount (the `ngwa.add-from-url` command). */
	initialAddUrl?: boolean;
	/** Open this row's sheet on mount (Health's "Reinstall from registry"). */
	initialSelectedId?: string | null;
}

type PendingAction = 'install' | 'update';

function errText(e: unknown): string {
	return e instanceof Error ? e.message : String(e);
}

/** True when `r` is a thenable (the handler returned a promise). */
function isPromise(r: unknown): r is Promise<unknown> {
	return Boolean(r) && typeof (r as Promise<unknown>).then === 'function';
}

const STORE_KINDS = [
	{ id: '*', label: 'all' },
	{ id: 'app', label: 'app' },
	{ id: 'engine', label: 'engine' },
	{ id: 'tool', label: 'tool' },
	{ id: 'skill', label: 'skill' },
	{ id: 'bundle', label: 'bundle' },
	// R57: catalog hooks (and MCP entries, when the catalog carries one).
	{ id: 'hook', label: 'hook' },
	{ id: 'mcp', label: 'mcp' },
];

/** R57 Source chips: where a row installs FROM. */
const STORE_SOURCES: Array<{ id: StoreSource; label: string }> = [
	{ id: 'registry', label: 'registry' },
	{ id: 'git', label: 'git' },
	{ id: 'npx', label: 'npx' },
];

/** A catalog row or a registry row, as the list filters them. */
interface FacetFields {
	kind: string;
	trust: string;
	source: StoreSource;
	text: string;
}

const registryFacets = (c: NgwaStoreEntry): FacetFields => ({
	kind: c.kind,
	trust: c.trustFacet,
	source: 'registry',
	text: `${c.name} ${c.displayName} ${c.description ?? ''}`,
});
const catalogFacets = (r: NgwaCatalogRow): FacetFields => ({
	// The catalog's signature covers the entry, not the files it points at.
	kind: r.kind,
	trust: 'unsigned',
	source: r.source,
	text: `${r.name} ${r.description ?? ''} ${r.url}`,
});

/** Does a row pass the active Kind / Trust / Source / search filters? */
function passesFacets(
	f: FacetFields,
	on: { kind: string; trust: string; source: StoreSource | null; search: string }
): boolean {
	if (on.kind !== '*' && f.kind !== on.kind) return false;
	if (on.trust !== '*' && f.trust !== on.trust) return false;
	if (on.source && f.source !== on.source) return false;
	const q = on.search.trim().toLowerCase();
	return !q || f.text.toLowerCase().includes(q);
}

const NO_PRIMITIVES: NgwaCatalogRow[] = [];
const NO_CATALOG: PrimitiveCatalogEntry[] = [];
const NO_VAULT: ClaudeStoreEntry[] = [];

const STORE_TRUSTS = [
	{ id: '*', label: 'all' },
	{ id: 'signed', label: 'signed' },
	{ id: 'unsigned', label: 'unsigned' },
];

const SIX_HOURS_MS = 6 * 60 * 60 * 1000;

/**
 * The detail file for one Store entry, keyed by name + version under the
 * registry namespace (so `useRefreshRegistry` invalidates it with the index).
 * Kept apart from `registryKeys.detail(name)`: that cache holds details parsed
 * by the registry-client's pinned schema, which drops `requires`/`signature`.
 * `enabled: false` (list rows) only reads whatever the cache already holds.
 */
function useStoreDetail(
	entry: NgwaStoreEntry,
	loadDetail: StoreDetailLoader | undefined,
	enabled: boolean
) {
	return useQuery({
		queryKey: [...registryKeys.detail(entry.name), 'store', entry.latestVersion],
		queryFn: ({ signal }) => {
			if (!loadDetail) throw new Error('No registry detail loader');
			return loadDetail(entry, signal);
		},
		enabled: enabled && Boolean(loadDetail),
		staleTime: SIX_HOURS_MS,
		retry: false,
	});
}

function manifestOf(version: StorePkgVersion | undefined): StoreManifest | null {
	return version?.manifest ?? null;
}

export function NgwaStoreSurface({
	catalog,
	isLoading = false,
	error = null,
	onRetry,
	loadDetail,
	activeProjectName,
	activeProjectId,
	onInstall,
	onUpdate,
	onUpdateAll,
	updateApprovals,
	primitives = NO_PRIMITIVES,
	catalogEntries = NO_CATALOG,
	vault = NO_VAULT,
	catalogStatus,
	catalogError = null,
	onRecheckCatalog,
	onInstallPrimitive,
	onUpdatePrimitive,
	onResolveSource,
	onInstallResolved,
	onOpenInstalled,
	initialAddUrl = false,
	initialSelectedId = null,
	disabledReason,
	primitiveDisabledReason,
	sourceBlock,
}: NgwaStoreSurfaceProps) {
	// The primitive controls (catalog rows, the Add-from-URL sheet) read their
	// own reason; absent, they share the package one.
	const primReason = primitiveDisabledReason ?? disabledReason;
	const [search, setSearch] = useState('');
	const [kindFilter, setKindFilter] = useState('*');
	const [trustFilter, setTrustFilter] = useState('*');
	const [sourceFilter, setSourceFilter] = useState<StoreSource | null>(null);
	const [selectedId, setSelectedId] = useState<string | null>(initialSelectedId);
	// R57: Add from URL occupies the sheet column; `seed` is the Source it
	// opens with (an empty search hands its query over), `n` remounts it.
	const [addUrl, setAddUrl] = useState<{ seed: string; n: number } | null>(
		initialAddUrl ? { seed: '', n: 0 } : null
	);
	const openAddUrl = (seed = '') => {
		setSelectedId(null);
		setAddUrl((a) => ({ seed, n: (a?.n ?? 0) + 1 }));
	};
	const selectRow = (id: string) => {
		setAddUrl(null);
		setSelectedId(id);
	};
	const [reviewOpen, setReviewOpen] = useState(false);
	const [trustReviewItem, setTrustReviewItem] = useState<NgwaItem | null>(null);
	// In-flight install/update per entry, and its last failure. Lifted here so
	// a row's Update and the sheet foot show the same real promise.
	const [pending, setPending] = useState<Record<string, PendingAction>>({});
	const [actionErrors, setActionErrors] = useState<Record<string, string>>({});
	const [updateAllBusy, setUpdateAllBusy] = useState(false);
	const [updateAllError, setUpdateAllError] = useState<string | null>(null);

	async function runAction(entry: NgwaStoreEntry, kind: PendingAction, fn: () => unknown) {
		if (pending[entry.id]) return;
		setActionErrors(({ [entry.id]: _drop, ...rest }) => rest);
		let result: unknown;
		try {
			result = fn();
		} catch (e) {
			setActionErrors((m) => ({ ...m, [entry.id]: errText(e) }));
			return;
		}
		if (!isPromise(result)) return;
		setPending((m) => ({ ...m, [entry.id]: kind }));
		try {
			await result;
		} catch (e) {
			// Held for approval is not a failure: the review opens instead.
			if (isNeedsApproval(e) && updateApprovals) updateApprovals.request(e.approvals);
			else setActionErrors((m) => ({ ...m, [entry.id]: errText(e) }));
		} finally {
			setPending(({ [entry.id]: _drop, ...rest }) => rest);
		}
	}

	// The last attempt per entry, so a failed row's Retry repeats it exactly
	// (same scope for an install).
	const lastAttempt = useRef<Record<string, () => void>>({});
	function attempt(entry: NgwaStoreEntry, kind: PendingAction, fn: () => unknown) {
		const go = () => void runAction(entry, kind, fn);
		lastAttempt.current[entry.id] = go;
		go();
	}
	const install = onInstall
		? (entry: NgwaStoreEntry, scope: StoreInstallScope) =>
				attempt(entry, 'install', () => onInstall(entry, scope))
		: undefined;
	const update = onUpdate
		? (entry: NgwaStoreEntry) => {
				// The foot is where progress and failures read, so open the row.
				selectRow(entry.id);
				attempt(entry, 'update', () => onUpdate(entry));
			}
		: undefined;
	const retry = (entry: NgwaStoreEntry) => lastAttempt.current[entry.id]?.();

	// R57 · Q4: a catalog install moves to its catalog pin. Same pending /
	// error bookkeeping as a registry update, keyed by the row id.
	async function runPrimitiveUpdate(row: NgwaCatalogRow) {
		if (!onUpdatePrimitive || pending[row.id]) return;
		setActionErrors(({ [row.id]: _drop, ...rest }) => rest);
		setPending((m) => ({ ...m, [row.id]: 'update' }));
		try {
			await onUpdatePrimitive(row);
		} catch (e) {
			setActionErrors((m) => ({ ...m, [row.id]: errText(e) }));
		} finally {
			setPending(({ [row.id]: _drop, ...rest }) => rest);
		}
	}
	const updatePrimitive = onUpdatePrimitive
		? (row: NgwaCatalogRow) => {
				selectRow(row.id);
				void runPrimitiveUpdate(row);
			}
		: undefined;

	const updateEntries = useMemo(() => catalog.filter((c) => c.isUpdate), [catalog]);
	// Held for approval and still an update (an approve elsewhere drops it).
	const approvalsPending = useMemo(
		() =>
			(updateApprovals?.pending ?? []).filter((p) =>
				updateEntries.some((e) => e.id === p.entry.id)
			),
		[updateApprovals?.pending, updateEntries]
	);
	// Q4: catalog installs whose pin moved join the strip; hook / MCP entries
	// can't be updated in place, so `isUpdate` is never set on them.
	const primitiveUpdates = useMemo(() => primitives.filter((r) => r.isUpdate), [primitives]);
	const updateCount = updateEntries.length + primitiveUpdates.length;
	const canUpdateAll =
		(updateEntries.length === 0 || Boolean(onUpdateAll)) &&
		(primitiveUpdates.length === 0 || Boolean(onUpdatePrimitive)) &&
		updateCount > 0;

	async function updateAll() {
		if (!canUpdateAll || updateAllBusy) return;
		setUpdateAllError(null);
		const failures: string[] = [];
		let registryRun: unknown;
		try {
			registryRun = updateEntries.length && onUpdateAll ? onUpdateAll(updateEntries) : undefined;
		} catch (e) {
			failures.push(errText(e));
		}
		const ids = [...updateEntries.map((e) => e.id), ...primitiveUpdates.map((r) => r.id)];
		if (!isPromise(registryRun) && primitiveUpdates.length === 0) {
			if (failures.length) setUpdateAllError(failures.join('; '));
			return;
		}
		setUpdateAllBusy(true);
		setPending((m) => ({ ...m, ...Object.fromEntries(ids.map((id) => [id, 'update'])) }));
		try {
			if (isPromise(registryRun)) {
				try {
					await registryRun;
				} catch (e) {
					if (isNeedsApproval(e) && updateApprovals) {
						// Held rows go to the review; only real failures stay here.
						updateApprovals.request(e.approvals);
						if (e.failures.length) {
							failures.push(
								`${e.failures.length} of ${updateEntries.length} failed — ${e.failures.join('; ')}`
							);
						}
					} else {
						failures.push(errText(e));
					}
				}
			}
			// One failing primitive must not stop the rest (batch-updater rule).
			for (const row of primitiveUpdates) {
				try {
					await onUpdatePrimitive?.(row);
				} catch (e) {
					failures.push(`${row.name}: ${errText(e)}`);
				}
			}
			if (failures.length) setUpdateAllError(failures.join('; '));
		} finally {
			setUpdateAllBusy(false);
			setPending((m) => {
				const next = { ...m };
				for (const id of ids) delete next[id];
				return next;
			});
		}
	}

	const filters = useMemo(
		() => ({ kind: kindFilter, trust: trustFilter, source: sourceFilter, search }),
		[kindFilter, trustFilter, sourceFilter, search]
	);
	const filtered = useMemo(
		() => catalog.filter((c) => passesFacets(registryFacets(c), filters)),
		[catalog, filters]
	);
	const filteredPrimitives = useMemo(
		() => primitives.filter((r) => passesFacets(catalogFacets(r), filters)),
		[primitives, filters]
	);

	const counts = useMemo(() => {
		const all = [...catalog.map(registryFacets), ...primitives.map(catalogFacets)];
		const kCounts: Record<string, number> = {};
		for (const k of STORE_KINDS) {
			kCounts[k.id] = all.filter((c) => k.id === '*' || c.kind === k.id).length;
		}
		const tCounts: Record<string, number> = {};
		for (const t of STORE_TRUSTS) {
			tCounts[t.id] = all.filter((c) => t.id === '*' || c.trust === t.id).length;
		}
		const sCounts: Record<string, number> = {};
		for (const s of STORE_SOURCES) sCounts[s.id] = all.filter((c) => c.source === s.id).length;
		return { kinds: kCounts, trusts: tCounts, sources: sCounts, total: all.length };
	}, [catalog, primitives]);

	// The Kind facet shows `hook` once the catalog lists (R57), `mcp` only when
	// it has one; everything else as D-02 locked it.
	const kindChips = STORE_KINDS.filter((k) => k.id !== 'mcp' || (counts.kinds.mcp ?? 0) > 0).filter(
		(k) => k.id !== 'hook' || primitives.length > 0 || (counts.kinds.hook ?? 0) > 0
	);

	// R57: the closure of each catalog row, read from the catalog (no fetch).
	const installedKeys = useMemo(() => new Set(vault.map((e) => `${e.kind}:${e.name}`)), [vault]);
	const closures = useMemo(() => {
		const m = new Map<string, ConsentDep[]>();
		for (const r of primitives) {
			m.set(r.id, resolveCatalogClosure(r.entry, catalogEntries, installedKeys));
		}
		return m;
	}, [primitives, catalogEntries, installedKeys]);

	// D-02: nothing is selected until the user picks a row — the sheet then
	// reads that row's closure and permissions. A selection survives the
	// filters (the design keeps the sheet open while you narrow the list).
	const selectedEntry = useMemo(
		() => (selectedId ? (catalog.find((c) => c.id === selectedId) ?? null) : null),
		[catalog, selectedId]
	);
	const selectedPrimitive = useMemo(
		() => (selectedId ? (primitives.find((r) => r.id === selectedId) ?? null) : null),
		[primitives, selectedId]
	);

	const projectLabel = activeProjectName || 'active project';
	// DEC-71: pkg scope has no Default project — with Default active a pkg's
	// target is personal. Primitives keep the project label: Ọba places them
	// in the project's own root.
	const pkgTargetLabel = pkgProjectTarget(activeProjectId, projectLabel);
	const shownCount = filtered.length + filteredPrimitives.length;
	const indexLine =
		catalogStatus === 'verified'
			? 'index + catalog signed'
			: catalogStatus === 'seed'
				? 'index signed · catalog bundled (remote absent)'
				: catalogStatus === 'error'
					? 'index signed · catalog unavailable'
					: catalogStatus === 'loading'
						? 'index signed · reading the catalog'
						: 'registry index signed';

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			{/* ── Updates strip ── */}
			{updateCount > 0 && (
				<div className="updates" data-updates>
					<Download className="h-4 w-4 flex-none" />
					<b data-updcount>
						{updateCount} {updateCount === 1 ? 'update' : 'updates'} available
					</b>
					{[
						...updateEntries.map((upd) => ({
							id: upd.id,
							text: `${upd.displayName} ${upd.version} → ${upd.latestVersion}`,
						})),
						...primitiveUpdates.map((r) => ({
							id: r.id,
							text: `${r.name} ${shortSha(r.installed?.version)} → ${shortSha(catalogPin(r.entry)?.sha ?? catalogPin(r.entry)?.hash?.slice(7, 14))}`,
						})),
					]
						.slice(0, 2)
						.map((w) => (
							<span key={w.id} className="who">
								{w.text}
							</span>
						))}
					{updateCount > 2 && <span className="who">+{updateCount - 2} more</span>}
					{canUpdateAll && (
						<span className="rt">
							{updateAllError && (
								<span className="upderr" role="alert" data-update-failures>
									{updateAllError}
								</span>
							)}
							{approvalsPending.length > 0 && (
								<>
									<span className="updappr" role="status" data-update-approvals>
										<ShieldAlert className="h-3 w-3" /> {approvalsPending.length}{' '}
										{approvalsPending.length === 1 ? 'needs' : 'need'} approval
									</span>
									<button
										type="button"
										className="btn"
										data-review-approvals
										onClick={() => updateApprovals?.review()}
									>
										Review
									</button>
								</>
							)}
							<button
								type="button"
								className="btn"
								data-update-all
								disabled={updateAllBusy}
								aria-busy={updateAllBusy || undefined}
								onClick={() => setReviewOpen(true)}
							>
								{updateAllBusy ? 'Updating…' : `Update all (${updateCount})`}
							</button>
						</span>
					)}
				</div>
			)}

			{/* ── Facet Bar ── */}
			<div className="facetbar">
				<div className="frow2">
					<div className="search" style={{ maxWidth: '340px' }}>
						<Search className="h-3.5 w-3.5" />
						<input
							type="text"
							placeholder="Search the registry…"
							aria-label="Search the registry"
							value={search}
							onChange={(e) => setSearch(e.target.value)}
						/>
					</div>
					{/* R57 · Q1: the entry point sits beside search — what you reach
					    for when search comes up empty, in the same eye-line. */}
					{onResolveSource && (
						<button
							type="button"
							className="btn addurl"
							data-addurl
							aria-expanded={addUrl !== null}
							title="Install a skill, agent or command from a git URL or an npx package"
							onClick={() => openAddUrl('')}
						>
							<Link2 className="h-3.5 w-3.5" /> Add from URL…
						</button>
					)}

					<span className="toolsep" />

					<span className="flabel">Kind</span>
					{kindChips.map((k) => {
						const on = kindFilter === k.id;
						return (
							<button
								key={k.id}
								type="button"
								data-kind={k.id}
								className={`chip ${on ? 'on' : ''}`}
								aria-pressed={on}
								onClick={() => setKindFilter(k.id)}
							>
								{k.label} <span className="n">{counts.kinds[k.id] ?? 0}</span>
							</button>
						);
					})}

					<span className="toolsep" />

					<span className="flabel" style={{ width: 'auto' }}>
						Trust
					</span>
					{STORE_TRUSTS.map((t) => {
						const on = trustFilter === t.id;
						return (
							<button
								key={t.id}
								type="button"
								data-trust={t.id}
								className={`chip ${on ? 'on' : ''}`}
								aria-pressed={on}
								onClick={() => setTrustFilter(t.id)}
							>
								{t.label} <span className="n">{counts.trusts[t.id] ?? 0}</span>
							</button>
						);
					})}

					{primitives.length > 0 && (
						<>
							<span className="toolsep" />
							<span className="flabel" style={{ width: 'auto' }}>
								Source
							</span>
							{STORE_SOURCES.map((s) => {
								const on = sourceFilter === s.id;
								return (
									<button
										key={s.id}
										type="button"
										data-source={s.id}
										className={`chip srcchip ${on ? 'on' : ''}`}
										aria-pressed={on}
										onClick={() => setSourceFilter(on ? null : s.id)}
									>
										{s.label} <span className="n">{counts.sources[s.id] ?? 0}</span>
									</button>
								);
							})}
						</>
					)}

					<span
						className="meta"
						style={{ marginLeft: 'auto' }}
						data-store-count
						title="The registry index and the curated catalog are both signed; the files they point at are not"
					>
						{indexLine}
						{!isLoading && !error && counts.total > 0
							? ` · ${shownCount} of ${counts.total} shown`
							: ''}
					</span>
				</div>
			</div>

			{/* ── Split List & Install Sheet ── */}
			<div className="split storesplit">
				<div className="listcol">
					<div className="sc flex-1 min-h-0" data-slist>
						{isLoading && (
							<LoadingState data-state="ngwa-store-loading" fill heading="Fetching the index" />
						)}

						{!isLoading && error && (
							<ErrorState
								data-state="ngwa-store-snapshot-error"
								fill
								heading="Can't read installed packages"
								body={error.message}
								action={onRetry ? { label: 'Retry', onClick: onRetry } : undefined}
							/>
						)}

						{!isLoading && !error && shownCount === 0 && (
							<div className="empty" data-store-empty>
								{primitives.length > 0
									? 'Nothing in the registry or the catalog matches. Both are fetched whole and filtered locally, so this is the real answer, not a slow query.'
									: 'Nothing in the registry matches. The index is fetched whole and filtered locally, so this is the real answer, not a slow query.'}
								{onResolveSource && (
									<div className="dacts">
										<button
											type="button"
											className="btn"
											data-addurl-empty
											onClick={() => openAddUrl(search.trim())}
										>
											<Link2 className="h-3.5 w-3.5" /> Add from URL…
										</button>
									</div>
								)}
							</div>
						)}

						{!isLoading &&
							!error &&
							filtered.map((entry) => (
								<StoreRow
									key={entry.id}
									entry={entry}
									selected={selectedEntry?.id === entry.id}
									loadDetail={loadDetail}
									onSelect={() => selectRow(entry.id)}
									onUpdate={update}
									busy={Boolean(pending[entry.id])}
									onRetry={lastAttempt.current[entry.id] ? () => retry(entry) : undefined}
									disabledReason={disabledReason}
								/>
							))}

						{/* R57: the signed catalog lists after the registry pkgs. */}
						{!isLoading &&
							!error &&
							filteredPrimitives.map((row) => (
								<CatalogStoreRow
									key={row.id}
									row={row}
									closure={closures.get(row.id) ?? []}
									selected={selectedPrimitive?.id === row.id}
									busy={Boolean(pending[row.id])}
									onSelect={() => selectRow(row.id)}
									onUpdate={updatePrimitive}
									disabledReason={primReason}
								/>
							))}

						{!isLoading && !error && catalogStatus === 'error' && (
							<div className="empty" data-catalog-unavailable role="status">
								<b>Registry unreachable.</b> The signed catalog is unavailable
								{catalogError ? ` — ${catalogError}` : ''}. None of its entries are listed: a
								catalog that fails to verify is never replaced by the bundled copy.
								{onRecheckCatalog && (
									<div className="dacts">
										<button type="button" className="btn" onClick={onRecheckCatalog}>
											Retry
										</button>
									</div>
								)}
							</div>
						)}
					</div>
				</div>

				{/* ── Install sheet ── */}
				<aside className="storesheet" data-storesheet role="region" aria-label="Install sheet">
					{addUrl ? (
						<AddUrlSheet
							key={`addurl:${addUrl.n}`}
							initialSource={addUrl.seed}
							projectLabel={projectLabel}
							catalog={catalogEntries}
							installedKeys={installedKeys}
							disabledReason={primReason}
							sourceBlock={sourceBlock}
							onClose={() => setAddUrl(null)}
							onResolve={onResolveSource}
							onInstall={onInstallResolved}
							onOpenInstalled={onOpenInstalled}
						/>
					) : selectedPrimitive ? (
						<CatalogSheet
							key={selectedPrimitive.id}
							row={selectedPrimitive}
							closure={closures.get(selectedPrimitive.id) ?? []}
							projectLabel={projectLabel}
							onClose={() => setSelectedId(null)}
							onInstall={onInstallPrimitive}
							onUpdate={updatePrimitive}
							updating={Boolean(pending[selectedPrimitive.id])}
							updateError={actionErrors[selectedPrimitive.id] ?? null}
							onRecheckCatalog={onRecheckCatalog}
							onOpenInstalled={onOpenInstalled}
							disabledReason={primReason}
						/>
					) : selectedEntry ? (
						<StoreSheet
							key={`${selectedEntry.id}@${selectedEntry.latestVersion}`}
							entry={selectedEntry}
							loadDetail={loadDetail}
							projectLabel={pkgTargetLabel}
							onClose={() => setSelectedId(null)}
							onInstall={install}
							onUpdate={update}
							pendingAction={pending[selectedEntry.id] ?? null}
							actionError={actionErrors[selectedEntry.id] ?? null}
							onRetry={
								lastAttempt.current[selectedEntry.id] ? () => retry(selectedEntry) : undefined
							}
							onReviewApproval={
								approvalsPending.some((p) => p.entry.id === selectedEntry.id)
									? () => updateApprovals?.review()
									: undefined
							}
							onReviewTrust={setTrustReviewItem}
							disabledReason={disabledReason}
						/>
					) : (
						<div className="sheetbody sc">
							<div className="empty" data-sheet-empty>
								Pick a row to read its closure, its permissions and its settings before you consent
								to anything.
							</div>
						</div>
					)}
				</aside>
			</div>

			<UpdatesReviewDialog
				open={reviewOpen}
				entries={updateEntries}
				primitives={primitiveUpdates}
				onCancel={() => setReviewOpen(false)}
				onConfirm={() => {
					setReviewOpen(false);
					void updateAll();
				}}
			/>

			<NgwaTrustSheet
				open={Boolean(trustReviewItem)}
				onOpenChange={(isOpen) => !isOpen && setTrustReviewItem(null)}
				item={trustReviewItem}
				mode="review"
			/>
		</div>
	);
}

// ─── Row ─────────────────────────────────────────────────────────────────────

function StoreRow({
	entry,
	selected,
	loadDetail,
	onSelect,
	onUpdate,
	busy,
	onRetry,
	disabledReason,
}: {
	entry: NgwaStoreEntry;
	selected: boolean;
	loadDetail: StoreDetailLoader | undefined;
	onSelect: () => void;
	onUpdate?: (entry: NgwaStoreEntry) => void;
	busy: boolean;
	onRetry?: () => void;
	disabledReason?: string;
}) {
	// This row's install / update progress, when the Store hook is driving
	// one: concurrent installs and Update all each show on their own row.
	const run = useInstallRun(entry.id);
	const showRun = run !== null && run.status !== 'done';
	// Cache-only: the selected row's sheet does the fetch; every other row
	// shows what has already been read, and says so when nothing has.
	const { data: detail } = useStoreDetail(entry, loadDetail, false);
	const manifest = manifestOf(detail);
	const publisher = manifest?.author?.name ?? null;

	return (
		<div
			className={`srow ${selected ? 'sel' : ''}`}
			data-id={entry.id}
			onClick={onSelect}
			role="button"
			tabIndex={0}
			aria-pressed={selected}
			aria-busy={busy || undefined}
			onKeyDown={(e) => {
				if (e.target !== e.currentTarget) return;
				if (e.key === 'Enter' || e.key === ' ') {
					e.preventDefault();
					onSelect();
				}
			}}
		>
			<div className="mark">{kindIcon(entry.kind)}</div>
			<div className="mid">
				<div className="l1">
					<span className="pkg">{entry.name}</span>
					<span className={`kind k-${entry.kind}`}>{entry.kind}</span>
					<span className="meta mono">{entry.latestVersion}</span>
					{publisher && <span className="meta">{publisher}</span>}
					<span className={`badge t-${entry.trustFacet}`}>
						<Shield className="h-3 w-3" />
						{entry.trustFacet}
					</span>
				</div>
				<div className="l2">{entry.description ?? 'No description.'}</div>
				<div className="l3" data-l3>
					<span className="tagp" data-closure>
						{closureLabel(entry.kind, manifest)}
					</span>
					<span className="tagp mono" data-asks>
						{asksLabel(entry.kind, manifest)}
					</span>
					{entry.alsoFrom && (
						// R57 · Q2: the signed catalog lists the same kind+name; this
						// row stands for both.
						<span
							className="tagp mono"
							data-also
							title={`The signed catalog also lists it: ${entry.alsoFrom.source} · ${entry.alsoFrom.url}`}
						>
							also: {entry.alsoFrom.source}
						</span>
					)}
				</div>
			</div>

			<div className="rt">
				{showRun && run ? (
					// biome-ignore lint/a11y/noStaticElementInteractions: stops row selection only
					<div onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
						<InstallProgressRow
							run={run}
							compact
							onCancel={() => void cancelStoreInstall(entry.id)}
							onRetry={onRetry}
						/>
					</div>
				) : entry.isUpdate ? (
					<button
						type="button"
						className="btn"
						disabled={!onUpdate || busy}
						title={onUpdate ? undefined : (disabledReason ?? NOT_AVAILABLE_ON_SERVER_YET)}
						onClick={(e) => {
							e.stopPropagation();
							onUpdate?.(entry);
						}}
					>
						{busy ? 'Updating…' : 'Update'}
					</button>
				) : entry.broken ? (
					// On disk but failed to register: say so, and route the fix
					// through the sheet (and its consent) like any install.
					<>
						<span className="badge t-review" data-broken title={entry.broken}>
							<AlertTriangle className="h-3 w-3" /> installed · failed to load
						</span>
						<button
							type="button"
							className="btn primary"
							data-reinstall
							onClick={(e) => {
								e.stopPropagation();
								onSelect();
							}}
						>
							Reinstall
						</button>
					</>
				) : entry.installedItem ? (
					<span className="badge t-builtin">
						<Check className="h-3 w-3" /> installed
					</span>
				) : (
					// D-02: Install opens the sheet — consent happens there, never
					// straight from the list.
					<button
						type="button"
						className="btn primary"
						onClick={(e) => {
							e.stopPropagation();
							onSelect();
						}}
					>
						Install
					</button>
				)}
			</div>
		</div>
	);
}

// ─── Sheet ───────────────────────────────────────────────────────────────────

function StoreSheet({
	entry,
	loadDetail,
	projectLabel,
	onClose,
	onInstall,
	onUpdate,
	pendingAction,
	actionError,
	onRetry,
	onReviewApproval,
	onReviewTrust,
	disabledReason,
}: {
	entry: NgwaStoreEntry;
	loadDetail: StoreDetailLoader | undefined;
	/** The project install target, or null when there is none (DEC-71). */
	projectLabel: string | null;
	onClose: () => void;
	onInstall?: (entry: NgwaStoreEntry, scope: StoreInstallScope) => void;
	onUpdate?: (entry: NgwaStoreEntry) => void;
	/** The install/update in flight for this entry (the real promise). */
	pendingAction: PendingAction | null;
	/** The last install/update failure for this entry. */
	actionError: string | null;
	/** Repeat the last failed install / update. */
	onRetry?: () => void;
	/** Set while this entry's update is held for approval: opens the review. */
	onReviewApproval?: () => void;
	onReviewTrust: (item: NgwaItem) => void;
	disabledReason?: string;
}) {
	const detailQuery = useStoreDetail(entry, loadDetail, true);
	const version = detailQuery.data ?? null;
	const manifest = version?.manifest ?? null;
	const consents = useMemo(() => (manifest ? consentGroups(manifest) : []), [manifest]);
	const [ticked, setTicked] = useState<Record<string, boolean>>({});
	const busy = pendingAction !== null;
	const storeRun = useInstallRun(entry.id);

	const title = manifest?.name ?? entry.displayName;
	const run = footRun(storeRun, pendingAction, actionError, title);
	const requires = manifest?.requires ?? [];
	const allTicked = consents.every((c) => ticked[c.id]);
	const size = formatBytes(version?.size);
	const settings = manifest?.settings?.schema ?? [];

	let installBlocked: string | null = null;
	if (!onInstall) installBlocked = disabledReason ?? NOT_AVAILABLE_ON_SERVER_YET;
	else if (!loadDetail) installBlocked = 'Permissions not read — the registry index has not loaded';
	else if (detailQuery.isLoading) installBlocked = 'Reading the manifest…';
	else if (!manifest) installBlocked = 'Permissions could not be read — retry first';
	else if (!allTicked)
		installBlocked = consentBlockedReason(
			consents.filter((c) => ticked[c.id]).length,
			consents.length
		);

	function install(scope: StoreInstallScope) {
		if (!onInstall || installBlocked || busy) return;
		onInstall(entry, scope);
	}

	const scopeLabel = (s: StoreInstallScope) =>
		s === 'personal' ? 'personal' : (projectLabel ?? 'personal');

	return (
		<>
			<div className="dhead">
				<div className="dtitle">
					<span className="ico">{kindIcon(entry.kind)}</span>
					<h2>{title}</h2>
					<span className={`kind k-${entry.kind}`}>{entry.kind}</span>
					<span className="v">{entry.latestVersion}</span>
					<span style={{ flex: 1 }} />
					<button type="button" className="iconbtn" aria-label="Close sheet" onClick={onClose}>
						<X className="h-3.5 w-3.5" />
					</button>
				</div>
				<div className="dsub">
					{manifest?.author?.name && (
						<span>
							publisher <b>{manifest.author.name}</b>
						</span>
					)}
					<span className="mono">{manifest?.id ?? entry.name}</span>
					{manifest && (
						<span>
							ikenga_api <b>{manifest.ikenga_api}</b>
						</span>
					)}
				</div>
			</div>

			<div className="sheetbody sc">
				<div className="subhead first">Overview</div>
				<p className="note" style={{ fontSize: 'var(--text-caption)' }}>
					{entry.description ?? 'No package description.'}
				</p>

				{detailQuery.isLoading && (
					<LoadingState data-state="ngwa-store-detail-loading" heading="Reading the manifest" />
				)}

				{!detailQuery.isLoading && !loadDetail && (
					<p className="note" data-detail-missing style={{ marginTop: 'var(--space-3)' }}>
						Closure and permissions not read — the registry index has not loaded, so this package’s
						detail file can’t be fetched yet.
					</p>
				)}

				{!detailQuery.isLoading &&
					detailQuery.error &&
					(typeof navigator !== 'undefined' && navigator.onLine === false ? (
						<OfflineState
							data-state="ngwa-store-detail-offline"
							heading="Registry unreachable"
							body="The closure and permissions live in this package’s detail file, which needs the network."
							action={{ label: 'Retry', onClick: () => void detailQuery.refetch() }}
						/>
					) : (
						<ErrorState
							data-state="ngwa-store-detail-error"
							heading="Couldn’t read the manifest"
							body={(detailQuery.error as Error).message}
							action={{ label: 'Retry', onClick: () => void detailQuery.refetch() }}
						/>
					))}

				{manifest && (
					<>
						{requires.length > 0 ? (
							<div data-requires>
								<div className="subhead">Requires — the closure, before you consent</div>
								{requires.map((r) => (
									<div className="drow" key={`${r.kind}:${r.name}`}>
										<span className="k2 mono">{r.name}</span>
										<span className="val">{r.kind}</span>
										<span className="rt meta">
											{scopeLabel('project')} · {r.source ?? 'catalog'}
											{r.ref ? ` @ ${r.ref}` : ''}
										</span>
									</div>
								))}
							</div>
						) : (
							<>
								<div className="subhead">Requires</div>
								<div className="drow">
									<span className="k2">Closure</span>
									<span className="val no">nothing — this installs alone</span>
								</div>
							</>
						)}

						<div className="subhead">Permissions</div>
						{consents.length > 0 ? (
							<div data-consents>
								<p className="kola">Share kola</p>
								<p className="note" style={{ marginBottom: 'var(--space-2)' }}>
									{title} asks for these before it runs. Tick each one to enable Install.
								</p>
								{consents.map((c) => (
									<div className="consent" key={c.id}>
										<label>
											<input
												type="checkbox"
												data-consent={c.id}
												checked={Boolean(ticked[c.id])}
												onChange={(e) => setTicked((t) => ({ ...t, [c.id]: e.target.checked }))}
											/>
											<span>
												<span className="b">{c.label}</span> <span className="d">{c.detail}</span>
											</span>
										</label>
									</div>
								))}
								<p className="consentnote">
									Consent is per install, because manifests are not signed yet.
								</p>
							</div>
						) : (
							<p className="note">
								{entry.kind === 'skill' || entry.kind === 'bundle'
									? `A ${entry.kind} declares intent and never grants itself anything, so there is nothing to consent to. Install is enabled.`
									: 'This manifest declares no permissions, so there is nothing to consent to. Install is enabled.'}
							</p>
						)}

						<div className="subhead">Trust</div>
						<div className="drow">
							<span className="k2">Signature</span>
							{manifest.signature ? (
								<span className="val yes">present — ed25519, verified at install</span>
							) : (
								<span className="val warn">
									absent — this manifest carries no ed25519 signature
								</span>
							)}
						</div>
						{manifest.author?.key && (
							<div className="drow">
								<span className="k2">Publisher key</span>
								<span className="val mono">{manifest.author.key}</span>
								<span className="rt meta">declared, unverified</span>
							</div>
						)}
						<div className="drow">
							<span className="k2">Registry index</span>
							<span className="val yes">signed · fetched over TLS</span>
						</div>
						{version?.integrity && (
							<div className="drow">
								<span className="k2">Integrity</span>
								<span className="val mono" style={{ overflow: 'hidden', textOverflow: 'ellipsis' }}>
									{version.integrity}
								</span>
							</div>
						)}

						<div className="subhead">Settings</div>
						{settings.length > 0 ? (
							settings.map((s) => {
								const unset = s.default === undefined || s.type === 'secret';
								return (
									<div className="drow" key={s.key}>
										<span className="k2">{s.key}</span>
										<span className={`val ${unset ? 'no' : 'mono'}`}>
											{unset ? 'unset' : String(s.default)}
										</span>
										<span className="rt meta">{s.type}</span>
									</div>
								);
							})
						) : (
							<div className="drow">
								<span className="k2">settings.schema</span>
								<span className="val no">none declared</span>
							</div>
						)}
					</>
				)}

				{entry.installedItem && (
					<button
						type="button"
						className="btn ghost"
						style={{ marginTop: 'var(--space-3)' }}
						onClick={() => entry.installedItem && onReviewTrust(entry.installedItem)}
					>
						<Shield className="h-3.5 w-3.5" /> Review permissions &amp; trust
					</button>
				)}
			</div>

			<div className="sheetfoot" data-sheetfoot>
				{run && (
					<InstallProgressRow
						run={run}
						onCancel={() => void cancelStoreInstall(entry.id)}
						onRetry={onRetry}
					/>
				)}
				{onReviewApproval && !busy && (
					<>
						<span className="note warn" role="status" data-needs-approval>
							needs approval — this version asks for new permissions
						</span>
						<button type="button" className="btn" onClick={onReviewApproval}>
							Review
						</button>
					</>
				)}
				{run && (run.status === 'running' || onRetry) ? null : entry.isUpdate ? (
					<button
						type="button"
						className="btn primary lg"
						disabled={!onUpdate || busy}
						aria-busy={busy || undefined}
						title={onUpdate ? undefined : (disabledReason ?? NOT_AVAILABLE_ON_SERVER_YET)}
						onClick={() => onUpdate?.(entry)}
					>
						Update {entry.version} → {entry.latestVersion}
					</button>
				) : entry.broken ? (
					<>
						<span className="note bad" data-broken-note title={entry.broken}>
							Installed · failed to load — reinstall
						</span>
						<InstallSplit
							projectLabel={projectLabel}
							blocked={installBlocked}
							busy={busy}
							onInstall={install}
							verb="Reinstall"
						/>
					</>
				) : entry.installedItem ? (
					<>
						<span className="badge t-builtin">
							<Check className="h-3 w-3" /> installed
						</span>
						<span className="note">Remove or move it from the Installed tab.</span>
					</>
				) : (
					<InstallSplit
						projectLabel={projectLabel}
						blocked={installBlocked}
						busy={busy}
						onInstall={install}
					/>
				)}
				{size && (
					<span className="note" style={{ marginLeft: 'auto' }}>
						{size}
					</span>
				)}
			</div>
		</>
	);
}

/**
 * What the sheet foot shows: the Store hook's progress run for this entry
 * when there is one, else a stand-in built from the surface's own promise
 * state (an `onInstall` that doesn't drive the progress store still gets an
 * indeterminate bar and a readable failure).
 */
function footRun(
	run: InstallRun | null,
	pendingAction: PendingAction | null,
	actionError: string | null,
	name: string
): InstallRun | null {
	if (run && run.status !== 'done') return run;
	const base = {
		key: name,
		name,
		verb: pendingAction ?? 'install',
		stage: 'resolving',
		percent: null,
		detail: null,
		step: null,
		cancellable: false,
		cancelRequested: false,
	} as const;
	if (pendingAction) {
		return {
			...base,
			status: 'running',
			label: pendingAction === 'update' ? 'Updating' : 'Installing',
			error: null,
		};
	}
	if (actionError) {
		const error = classifyInstallError(actionError, name);
		return {
			...base,
			status: error.kind === 'cancelled' ? 'cancelled' : 'failed',
			label: '',
			error,
		};
	}
	return null;
}

// ─── Updates review ──────────────────────────────────────────────────────────

/** D-02 §5 #23: "Review each" folded into one button that opens this list;
 *  nothing is applied until the user confirms here. */
function UpdatesReviewDialog({
	open,
	entries,
	primitives,
	onCancel,
	onConfirm,
}: {
	open: boolean;
	entries: NgwaStoreEntry[];
	primitives: NgwaCatalogRow[];
	onCancel: () => void;
	onConfirm: () => void;
}) {
	const n = entries.length + primitives.length;
	return (
		<Dialog open={open} onOpenChange={(o) => !o && onCancel()}>
			{open && (
				<DialogContent data-ngwa-confirm data-updates-review showCloseButton={false}>
					<DialogHeader>
						<DialogTitle>
							Review {n} update{n === 1 ? '' : 's'}
						</DialogTitle>
						<DialogDescription asChild>
							<div className="ngwa-confirm-body">
								{entries.map((e) => (
									<div className="drow" key={e.id}>
										<span className="k2">{e.name}</span>
										<span className="val mono">
											{e.version} → {e.latestVersion}
										</span>
									</div>
								))}
								{primitives.map((r) => {
									const pin = catalogPin(r.entry);
									return (
										<div className="drow" key={r.id} data-primitive-update={r.name}>
											<span className="k2">{r.name}</span>
											<span className="val mono">
												{shortSha(r.installed?.version)} → {shortSha(pin?.sha ?? null)}
											</span>
											<span className="rt meta">moved by the signed catalog</span>
										</div>
									);
								})}
								<p className="note">
									Each one is fetched, verified and re-registered. Nothing is applied until you
									confirm.
								</p>
							</div>
						</DialogDescription>
					</DialogHeader>
					<DialogFooter>
						<button type="button" className="chip" onClick={onCancel}>
							Cancel
						</button>
						<button type="button" className="chip on" onClick={onConfirm}>
							Update all ({n})
						</button>
					</DialogFooter>
				</DialogContent>
			)}
		</Dialog>
	);
}
