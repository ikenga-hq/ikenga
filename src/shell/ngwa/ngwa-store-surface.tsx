// Ngwa Store Surface (WP-15 / locked D-02).
//
// Registry catalog browsing, package installation, and updates:
// - Updates available banner with "Update all (N)"
// - Kind & Trust facet chips
// - Install ▾ split dropdown for personal vs project scope
// - Token-based styling

import { useState, useMemo } from 'react';
import {
	Download,
	Search,
	Shield,
	Check,
	ChevronDown,
} from 'lucide-react';
import { LoadingState, OfflineState } from '@/components/states';
import type { NgwaStoreEntry } from '@/lib/ngwa/enrichment';
import type { NgwaItem } from '@ikenga/contract';
import { kindIcon } from './ngwa-list';
import { NgwaTrustSheet } from './ngwa-trust-sheet';
import './ngwa.css';

export interface NgwaStoreSurfaceProps {
	catalog: NgwaStoreEntry[];
	isLoading?: boolean;
	error?: Error | null;
	/** D-07 offline state's one next action. */
	onRetry?: () => void;
	onInstall?: (entry: NgwaStoreEntry, scope: 'personal' | 'project') => void;
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

export function NgwaStoreSurface({
	catalog,
	isLoading = false,
	error = null,
	onRetry,
	onInstall,
	onUpdate,
	onUpdateAll,
}: NgwaStoreSurfaceProps) {
	const [search, setSearch] = useState('');
	const [kindFilter, setKindFilter] = useState('*');
	const [trustFilter, setTrustFilter] = useState('*');
	const [selectedId, setSelectedId] = useState<string | null>(null);
	const [openInstallMenuId, setOpenInstallMenuId] = useState<string | null>(null);
	const [trustReviewItem, setTrustReviewItem] = useState<NgwaItem | null>(null);

	// Updates available
	const updateEntries = useMemo(() => {
		return catalog.filter((c) => c.isUpdate);
	}, [catalog]);

	// Filtered catalog
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

	// Counts
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

	const selectedEntry = useMemo(() => {
		if (!filtered.length) return null;
		if (!selectedId) return filtered[0];
		return filtered.find((c) => c.id === selectedId) ?? filtered[0];
	}, [filtered, selectedId]);

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			{/* ── Updates Available Banner ── */}
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
					<div className="rt">
						{onUpdateAll && (
							<button
								type="button"
								className="chip on"
								onClick={() => onUpdateAll(updateEntries)}
							>
								Update all ({updateEntries.length})
							</button>
						)}
					</div>
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
						const cnt = counts.kinds[k.id] ?? 0;
						return (
							<button
								key={k.id}
								type="button"
								data-kind={k.id}
								className={`chip ${on ? 'on' : ''}`}
								aria-pressed={on}
								onClick={() => setKindFilter(k.id)}
							>
								{k.label} <span className="n">{cnt}</span>
							</button>
						);
					})}

					<span className="toolsep" />

					<span className="flabel" style={{ width: 'auto' }}>
						Trust
					</span>
					{STORE_TRUSTS.map((t) => {
						const cnt =
							t.id === '*'
								? catalog.length
								: catalog.filter((c) => c.trustFacet === t.id).length;
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
								{t.label} <span className="n">{cnt}</span>
							</button>
						);
					})}

					<span className="meta" style={{ marginLeft: 'auto' }}>
						registry index signed
					</span>
				</div>
			</div>

			{/* ── Split List & Install Sheet ── */}
			<div className="split">
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
							<div className="empty">No registry equipment matches these filters.</div>
						)}

						{!isLoading &&
							filtered.map((entry) => {
								const isSelected = selectedEntry?.id === entry.id;
								return (
									<div
										key={entry.id}
										className={`srow ${isSelected ? 'sel' : ''}`}
										onClick={() => setSelectedId(entry.id)}
										role="button"
										tabIndex={0}
										onKeyDown={(e) => {
											if (e.key === 'Enter' || e.key === ' ') {
												setSelectedId(entry.id);
											}
										}}
									>
										<div className="mark">{kindIcon(entry.kind)}</div>
										<div className="mid">
											<div className="l1">
												<span className="pkg">{entry.displayName}</span>
												<span className="tagp mono">v{entry.latestVersion}</span>
												<span className={`kind k-${entry.kind}`}>{entry.kind}</span>
												<span className={`badge t-${entry.trustFacet}`}>
													<Shield className="h-3 w-3" />
													{entry.trustFacet}
												</span>
											</div>
											<div className="l2">{entry.description ?? 'No description.'}</div>
										</div>

										<div className="rt" onClick={(e) => e.stopPropagation()}>
											{entry.isUpdate ? (
												<button
													type="button"
													className="chip on"
													onClick={() => onUpdate?.(entry)}
												>
													<Download className="h-3 w-3" /> Update to {entry.latestVersion}
												</button>
											) : entry.installedItem ? (
												<span className="badge t-builtin">
													<Check className="h-3 w-3" /> Installed
												</span>
											) : (
												<div className="relative inline-flex">
													<button
														type="button"
														aria-label="Choose install scope"
														className="chip on flex items-center gap-1"
														onClick={() =>
															setOpenInstallMenuId(
																openInstallMenuId === entry.id ? null : entry.id
															)
														}
													>
														<Download className="h-3 w-3" /> Install
														<ChevronDown className="h-3 w-3 ml-0.5" />
													</button>
													{openInstallMenuId === entry.id && (
														<div
															className="menu"
															style={{
																position: 'absolute',
																right: 0,
																top: '100%',
																marginTop: '4px',
																zIndex: 90,
															}}
														>
															<div className="mgroup">Install Scope</div>
															<button
																type="button"
																className="mitem"
																onClick={() => {
																	setOpenInstallMenuId(null);
																	onInstall?.(entry, 'personal');
																}}
															>
																Personal scope (~/.claude)
															</button>
															<button
																type="button"
																className="mitem"
																onClick={() => {
																	setOpenInstallMenuId(null);
																	onInstall?.(entry, 'project');
																}}
															>
																Active project (.claude)
															</button>
														</div>
													)}
												</div>
											)}
										</div>
									</div>
								);
							})}
					</div>
				</div>

				{/* ── Install / Detail Sheet ── */}
				<aside className="storesheet" role="region" aria-label="Install sheet">
					{selectedEntry ? (
						<div className="sheetbody sc">
							<div className="dtitle">
								<h2>{selectedEntry.displayName}</h2>
								<span className="v">v{selectedEntry.latestVersion}</span>
								<span className={`badge t-${selectedEntry.trustFacet}`}>
									<Shield className="h-3 w-3" />
									{selectedEntry.trustFacet}
								</span>
							</div>

							<p className="note" style={{ marginTop: 'var(--space-2)' }}>
								{selectedEntry.description ?? 'No package description.'}
							</p>

							<div className="drow" style={{ marginTop: 'var(--space-3)' }}>
								<span className="k2">Package ID</span>
								<span className="val mono text-xs">{selectedEntry.id}</span>
							</div>

							<div className="drow">
								<span className="k2">Kind</span>
								<span className="val">{selectedEntry.kind}</span>
							</div>

							<div className="drow">
								<span className="k2">Status</span>
								<span className="val">
									{selectedEntry.isUpdate
										? `Update available (${selectedEntry.version} → ${selectedEntry.latestVersion})`
										: selectedEntry.installedItem
										? `Installed (${selectedEntry.version})`
										: 'Not installed'}
								</span>
							</div>

							<div className="subhead">Provenance</div>
							<div className="drow">
								<span className="k2">Source</span>
								<span className="val">Ikenga Registry (npm / tarball)</span>
							</div>
							<div className="drow">
								<span className="k2">Integrity</span>
								<span className="val mono text-xs">
									{'integrity' in selectedEntry.registryEntry ? String((selectedEntry.registryEntry as Record<string, unknown>).integrity) : 'minisign-verified index'}
								</span>
							</div>

							{selectedEntry.installedItem && (
								<button
									type="button"
									className="btn ghost text-xs mt-3 w-full"
									onClick={() => setTrustReviewItem(selectedEntry.installedItem)}
								>
									<Shield className="h-3.5 w-3.5 mr-1" /> Review permissions & trust
								</button>
							)}
						</div>
					) : (
						<div className="empty">Select an item to view details.</div>
					)}
				</aside>
			</div>

			<NgwaTrustSheet
				open={Boolean(trustReviewItem)}
				onOpenChange={(isOpen) => !isOpen && setTrustReviewItem(null)}
				item={trustReviewItem}
				mode="review"
			/>
		</div>
	);
}
