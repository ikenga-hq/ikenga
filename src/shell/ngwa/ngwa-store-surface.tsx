// Ngwa Store Surface (WP-15 / locked D-02).
//
// Registry catalog browsing, package installation, and updates:
// - Updates strip: "Update all (N)" opens a review of each pending update
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

import { useEffect, useMemo, useRef, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { Check, ChevronDown, Download, Search, Shield, X } from 'lucide-react';
import { ErrorState, LoadingState, OfflineState } from '@/components/states';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import type { NgwaStoreEntry } from '@/lib/ngwa/enrichment';
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
import './ngwa.css';

export type StoreInstallScope = 'personal' | 'project';

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
	isLoading?: boolean;
	error?: Error | null;
	/** D-07 offline state's one next action. */
	onRetry?: () => void;
	/**
	 * Detail-file loader for the selected row. Absent (e.g. the index hasn't
	 * loaded) → the sheet can't read the closure or permissions, so Install
	 * stays disabled.
	 */
	loadDetail?: StoreDetailLoader;
	/** Display name of the active project — the default install target. */
	activeProjectName?: string;
	onInstall?: (entry: NgwaStoreEntry, scope: StoreInstallScope) => void | Promise<unknown>;
	onUpdate?: (entry: NgwaStoreEntry) => void;
	onUpdateAll?: (entries: NgwaStoreEntry[]) => void;
}

const STORE_KINDS = [
	{ id: '*', label: 'all' },
	{ id: 'app', label: 'app' },
	{ id: 'engine', label: 'engine' },
	{ id: 'tool', label: 'tool' },
	{ id: 'skill', label: 'skill' },
	{ id: 'bundle', label: 'bundle' },
];

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
	onInstall,
	onUpdate,
	onUpdateAll,
}: NgwaStoreSurfaceProps) {
	const [search, setSearch] = useState('');
	const [kindFilter, setKindFilter] = useState('*');
	const [trustFilter, setTrustFilter] = useState('*');
	const [selectedId, setSelectedId] = useState<string | null>(null);
	const [reviewOpen, setReviewOpen] = useState(false);
	const [trustReviewItem, setTrustReviewItem] = useState<NgwaItem | null>(null);

	const updateEntries = useMemo(() => catalog.filter((c) => c.isUpdate), [catalog]);

	const filtered = useMemo(() => {
		return catalog.filter((c) => {
			if (kindFilter !== '*' && c.kind !== kindFilter) return false;
			if (trustFilter !== '*' && c.trustFacet !== trustFilter) return false;
			if (search.trim()) {
				const q = search.toLowerCase();
				const text = `${c.name} ${c.displayName} ${c.description ?? ''}`.toLowerCase();
				if (!text.includes(q)) return false;
			}
			return true;
		});
	}, [catalog, kindFilter, trustFilter, search]);

	const counts = useMemo(() => {
		const kCounts: Record<string, number> = {};
		for (const k of STORE_KINDS) {
			kCounts[k.id] = catalog.filter((c) => k.id === '*' || c.kind === k.id).length;
		}
		const tCounts: Record<string, number> = {};
		for (const t of STORE_TRUSTS) {
			tCounts[t.id] = catalog.filter((c) => t.id === '*' || c.trustFacet === t.id).length;
		}
		return { kinds: kCounts, trusts: tCounts };
	}, [catalog]);

	// D-02: nothing is selected until the user picks a row — the sheet then
	// reads that row's closure and permissions. A selection survives the
	// filters (the design keeps the sheet open while you narrow the list).
	const selectedEntry = useMemo(
		() => (selectedId ? (catalog.find((c) => c.id === selectedId) ?? null) : null),
		[catalog, selectedId]
	);

	const projectLabel = activeProjectName || 'active project';

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			{/* ── Updates strip ── */}
			{updateEntries.length > 0 && (
				<div className="updates" data-updates>
					<Download className="h-4 w-4 flex-none" />
					<b data-updcount>
						{updateEntries.length} {updateEntries.length === 1 ? 'update' : 'updates'} available
					</b>
					{updateEntries.slice(0, 2).map((upd) => (
						<span key={upd.id} className="who">
							{upd.displayName} {upd.version} → {upd.latestVersion}
						</span>
					))}
					{updateEntries.length > 2 && (
						<span className="who">+{updateEntries.length - 2} more</span>
					)}
					{onUpdateAll && (
						<span className="rt">
							<button
								type="button"
								className="btn"
								data-update-all
								onClick={() => setReviewOpen(true)}
							>
								Update all ({updateEntries.length})
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

					<span className="toolsep" />

					<span className="flabel">Kind</span>
					{STORE_KINDS.map((k) => {
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

					<span className="meta" style={{ marginLeft: 'auto' }} data-store-count>
						registry index signed
						{!isLoading && !error && catalog.length > 0
							? ` · ${filtered.length} of ${catalog.length} shown`
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
							<OfflineState
								data-state="ngwa-store-offline"
								fill
								heading="Registry unreachable"
								body="Everything installed still runs. Only browsing and installing new packages needs the network."
								action={onRetry ? { label: 'Retry', onClick: onRetry } : undefined}
							/>
						)}

						{!isLoading && !error && filtered.length === 0 && (
							<div className="empty">
								Nothing in the registry matches. The index is fetched whole and filtered locally, so
								this is the real answer, not a slow query.
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
									onSelect={() => setSelectedId(entry.id)}
									onUpdate={onUpdate}
								/>
							))}
					</div>
				</div>

				{/* ── Install sheet ── */}
				<aside className="storesheet" data-storesheet role="region" aria-label="Install sheet">
					{selectedEntry ? (
						<StoreSheet
							key={`${selectedEntry.id}@${selectedEntry.latestVersion}`}
							entry={selectedEntry}
							loadDetail={loadDetail}
							projectLabel={projectLabel}
							onClose={() => setSelectedId(null)}
							onInstall={onInstall}
							onUpdate={onUpdate}
							onReviewTrust={setTrustReviewItem}
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
				onCancel={() => setReviewOpen(false)}
				onConfirm={() => {
					setReviewOpen(false);
					onUpdateAll?.(updateEntries);
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
}: {
	entry: NgwaStoreEntry;
	selected: boolean;
	loadDetail: StoreDetailLoader | undefined;
	onSelect: () => void;
	onUpdate?: (entry: NgwaStoreEntry) => void;
}) {
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
				</div>
			</div>

			<div className="rt">
				{entry.isUpdate ? (
					<button
						type="button"
						className="btn"
						onClick={(e) => {
							e.stopPropagation();
							onUpdate?.(entry);
						}}
					>
						Update
					</button>
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
	onReviewTrust,
}: {
	entry: NgwaStoreEntry;
	loadDetail: StoreDetailLoader | undefined;
	projectLabel: string;
	onClose: () => void;
	onInstall?: NgwaStoreSurfaceProps['onInstall'];
	onUpdate?: (entry: NgwaStoreEntry) => void;
	onReviewTrust: (item: NgwaItem) => void;
}) {
	const detailQuery = useStoreDetail(entry, loadDetail, true);
	const version = detailQuery.data ?? null;
	const manifest = version?.manifest ?? null;
	const consents = useMemo(() => (manifest ? consentGroups(manifest) : []), [manifest]);
	const [ticked, setTicked] = useState<Record<string, boolean>>({});
	const [menuOpen, setMenuOpen] = useState(false);
	const [busy, setBusy] = useState(false);
	const [installError, setInstallError] = useState<string | null>(null);
	const menuRef = useRef<HTMLDivElement | null>(null);

	// Dismiss the install-scope popover on Escape or a click outside it,
	// matching the Scopes surface's cell popover.
	useEffect(() => {
		if (!menuOpen) return;
		function onKey(e: KeyboardEvent) {
			if (e.key === 'Escape') {
				e.preventDefault();
				setMenuOpen(false);
			}
		}
		function onDown(e: MouseEvent) {
			if (menuRef.current?.contains(e.target as Node)) return;
			setMenuOpen(false);
		}
		document.addEventListener('keydown', onKey);
		document.addEventListener('mousedown', onDown);
		return () => {
			document.removeEventListener('keydown', onKey);
			document.removeEventListener('mousedown', onDown);
		};
	}, [menuOpen]);

	const title = manifest?.name ?? entry.displayName;
	const requires = manifest?.requires ?? [];
	const allTicked = consents.every((c) => ticked[c.id]);
	const size = formatBytes(version?.size);
	const settings = manifest?.settings?.schema ?? [];

	let installBlocked: string | null = null;
	if (!onInstall) installBlocked = 'Install is not available here';
	else if (!loadDetail) installBlocked = 'Permissions not read — the registry index has not loaded';
	else if (detailQuery.isLoading) installBlocked = 'Reading the manifest…';
	else if (!manifest) installBlocked = 'Permissions could not be read — retry first';
	else if (!allTicked) installBlocked = 'Tick every consent above first';

	async function install(scope: StoreInstallScope) {
		setMenuOpen(false);
		if (!onInstall || installBlocked) return;
		setInstallError(null);
		const result = onInstall(entry, scope);
		if (result && typeof (result as Promise<unknown>).then === 'function') {
			setBusy(true);
			try {
				await result;
			} catch (e) {
				setInstallError(e instanceof Error ? e.message : String(e));
			} finally {
				setBusy(false);
			}
		}
	}

	const scopeLabel = (s: StoreInstallScope) => (s === 'personal' ? 'personal' : projectLabel);

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
				{entry.isUpdate ? (
					<button type="button" className="btn primary lg" onClick={() => onUpdate?.(entry)}>
						Update {entry.version} → {entry.latestVersion}
					</button>
				) : entry.installedItem ? (
					<>
						<span className="badge t-builtin">
							<Check className="h-3 w-3" /> installed
						</span>
						<span className="note">Remove or move it from the Installed tab.</span>
					</>
				) : (
					<>
						{busy && (
							<span className="emberbar" role="status">
								<i /> Registering
							</span>
						)}
						{installError && (
							<span className="note bad" role="alert">
								{installError}
							</span>
						)}
						<div className="installsplit" ref={menuRef}>
							<button
								type="button"
								className="btn primary lg"
								data-install
								disabled={Boolean(installBlocked) || busy}
								aria-busy={busy || undefined}
								title={installBlocked ?? undefined}
								onClick={() => void install('project')}
							>
								Install to {projectLabel}
							</button>
							<button
								type="button"
								className="btn primary lg caret"
								aria-label="Choose install scope"
								aria-haspopup="menu"
								aria-expanded={menuOpen}
								title={installBlocked ?? 'Choose an install scope'}
								disabled={Boolean(installBlocked) || busy}
								onClick={() => setMenuOpen((o) => !o)}
							>
								<ChevronDown className="h-3.5 w-3.5" />
							</button>
							{menuOpen && (
								<div className="cellpop storepop up" role="menu" aria-label="Install scope">
									<div className="mgroup">Install scope</div>
									<button
										type="button"
										role="menuitem"
										className="mitem"
										onClick={() => void install('project')}
									>
										Install to {projectLabel} <span className="msub">default here</span>
									</button>
									<button
										type="button"
										role="menuitem"
										className="mitem"
										onClick={() => void install('personal')}
									>
										Install to personal <span className="msub">~/.claude</span>
									</button>
								</div>
							)}
						</div>
					</>
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

// ─── Updates review ──────────────────────────────────────────────────────────

/** D-02 §5 #23: "Review each" folded into one button that opens this list;
 *  nothing is applied until the user confirms here. */
function UpdatesReviewDialog({
	open,
	entries,
	onCancel,
	onConfirm,
}: {
	open: boolean;
	entries: NgwaStoreEntry[];
	onCancel: () => void;
	onConfirm: () => void;
}) {
	return (
		<Dialog open={open} onOpenChange={(o) => !o && onCancel()}>
			{open && (
				<DialogContent data-ngwa-confirm data-updates-review showCloseButton={false}>
					<DialogHeader>
						<DialogTitle>
							Review {entries.length} update{entries.length === 1 ? '' : 's'}
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
							Update all ({entries.length})
						</button>
					</DialogFooter>
				</DialogContent>
			)}
		</Dialog>
	);
}
