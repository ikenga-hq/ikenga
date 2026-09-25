// The unified pkg surface — titlebar + trust banner + catalog body + loupe
// sheet + install sheet.
//
// Filters and counts live in the Packages-mode sidebar (deep-linked via
// ?filter=). The surface itself stays tightly focused on the catalog rows
// + per-pkg detail.

import { useEffect, useMemo, useState } from 'react';
import { AlertTriangle, ArrowUp, Loader2 } from 'lucide-react';
import { useNavigate } from '@tanstack/react-router';
import { Banner } from '@/components/ui/banner';
import { Button } from '@/components/ui/button';
import { FeedbackState } from '@/components/ui/feedback-state';
import { LoadingState, OfflineState } from '@/components/states';
import type { PkgRowV2 } from '@/lib/pkgs/use-derived';
import { usePkgsDerived } from '@/lib/pkgs/use-derived';
import { useUpdatePkgs, type UpdateFailure, type UpdateProgress } from '@/lib/pkgs/use-update-pkgs';
import { PkgGroup } from './pkg-row';
import { PkgInstallSheet } from './pkg-install-sheet';
import { PkgLoupe, type LoupeTab } from './pkg-loupe';
import { PkgsTitlebar } from './pkgs-titlebar';
import { PkgsTrustBanner } from './pkgs-trust-banner';

export type FilterKey = 'all' | 'installed' | 'updates' | 'store' | 'review' | 'disabled';
export type InstallTab = 'manifest-url' | 'local-path' | 'registry';

export interface PkgsSurfaceProps {
	/** Initial filter seeded from the URL (?filter=) or sidebar click. */
	initialFilter?: FilterKey;
	/** Open the install sheet on mount, pre-focused on this tab. From
	 *  ?install= search param (sidebar's "Install from path" item). */
	initialInstallTab?: InstallTab;
}

export function PkgsSurface({ initialFilter = 'all', initialInstallTab }: PkgsSurfaceProps = {}) {
	const d = usePkgsDerived();
	const navigate = useNavigate();

	const [filter, setFilter] = useState<FilterKey>(initialFilter);
	// Re-sync if the parent route flips the search param after mount (sidebar
	// click while already on /packages).
	useEffect(() => {
		setFilter(initialFilter);
	}, [initialFilter]);
	const [query, setQuery] = useState('');
	const [loupePkg, setLoupePkg] = useState<PkgRowV2 | null>(null);
	const [loupeTab, setLoupeTab] = useState<LoupeTab>('overview');
	const [installOpen, setInstallOpen] = useState(Boolean(initialInstallTab));
	const [installTab, setInstallTab] = useState<InstallTab>(initialInstallTab ?? 'manifest-url');
	const [installPkg, setInstallPkg] = useState<PkgRowV2 | null>(null);
	// Open + tab-focus the sheet when the URL param flips (sidebar click while
	// already on /packages). Clear the param after consuming so a refresh
	// doesn't re-open the sheet involuntarily.
	useEffect(() => {
		if (initialInstallTab) {
			setInstallPkg(null);
			setInstallTab(initialInstallTab);
			setInstallOpen(true);
			void navigate({
				to: '/packages',
				search: (prev) => {
					// Cast: TanStack widens `prev` to the union of all route search
					// schemas. Re-narrow here so the spread still produces a valid
					// /packages search shape.
					const p = prev as { filter?: string; install?: string };
					return {
						filter: p.filter as
							| 'all'
							| 'installed'
							| 'updates'
							| 'store'
							| 'review'
							| 'disabled'
							| undefined,
						install: undefined,
					};
				},
				replace: true,
			});
		}
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, [initialInstallTab]);

	const visible = useMemo(() => {
		const q = query.trim().toLowerCase();
		let rows: PkgRowV2[] = d.rows;
		if (filter === 'installed') rows = d.installed;
		else if (filter === 'store') rows = d.registry;
		else if (filter === 'updates') rows = d.updates;
		else if (filter === 'review')
			rows = [...d.trust, ...d.violations.filter((r) => !d.trust.find((t) => t.id === r.id))];
		else if (filter === 'disabled') rows = d.installed.filter((r) => !r.enabled);
		if (q) {
			rows = rows.filter(
				(r) =>
					r.name.toLowerCase().includes(q) ||
					r.id.toLowerCase().includes(q) ||
					r.routes.some((rt) => rt.toLowerCase().includes(q))
			);
		}
		return rows;
	}, [d, filter, query]);

	const groups = useMemo(() => {
		// Reuse the origin grouping from the artifact for the unified list,
		// but only show groups that contain visible rows after filtering.
		const byOrigin: Record<string, PkgRowV2[]> = {
			'Built-in · com.ikenga': [],
			Engine: [],
			'User · com.royalti': [],
			'Available in registry': [],
		};
		for (const row of visible) {
			if (row.origin === 'builtin') byOrigin['Built-in · com.ikenga'].push(row);
			else if (row.origin === 'engine') byOrigin.Engine.push(row);
			else if (row.origin === 'user') byOrigin['User · com.royalti'].push(row);
			else byOrigin['Available in registry'].push(row);
		}
		return byOrigin;
	}, [visible]);

	const openLoupe = (row: PkgRowV2, tab: LoupeTab = 'overview') => {
		setLoupePkg(row);
		setLoupeTab(tab);
	};

	const updatePkgs = useUpdatePkgs();
	const [updateAllProgress, setUpdateAllProgress] = useState<UpdateProgress | null>(null);
	const [updateFailures, setUpdateFailures] = useState<UpdateFailure[]>([]);
	const updateAll = () => {
		if (!d.updates.length || updatePkgs.isPending) return;
		setUpdateFailures([]);
		updatePkgs.mutate(
			{ rows: d.updates, onProgress: setUpdateAllProgress },
			{
				onSuccess: (res) => setUpdateFailures(res.failed),
				// A thrown mutation (e.g. registry index unavailable) bypasses the
				// per-row failure list — surface it instead of swallowing it.
				onError: (e) =>
					setUpdateFailures([
						{ id: '*', name: 'Update all', error: e instanceof Error ? e.message : String(e) },
					]),
				onSettled: () => setUpdateAllProgress(null),
			}
		);
	};

	return (
		<div className="flex h-full flex-col bg-background">
			<PkgsTitlebar
				d={d}
				query={query}
				onQueryChange={setQuery}
				onInstallPkg={() => {
					setInstallPkg(null);
					setInstallOpen(true);
				}}
			/>
			<PkgsTrustBanner
				d={d}
				onReview={() => {
					// URL-sync so a refresh preserves the focused filter and the
					// sidebar's active highlight tracks the banner click.
					void navigate({ to: '/packages', search: { filter: 'review' } });
				}}
			/>
			{d.updates.length > 0 && (
				<Banner
					tone="warning"
					icon={<ArrowUp />}
					className="px-6 py-2.5"
					actions={
						<Button
							size="sm"
							className="bg-[var(--achievement)] text-[var(--achievement-soft)] hover:bg-[var(--achievement)]/90"
							disabled={updatePkgs.isPending}
							onClick={updateAll}
						>
							{updatePkgs.isPending ? (
								<Loader2 aria-hidden="true" className="mr-1.5 size-3.5 animate-spin" />
							) : (
								<ArrowUp aria-hidden="true" className="mr-1.5 size-3.5" />
							)}
							{updatePkgs.isPending ? 'Updating…' : 'Update all'}
						</Button>
					}
				>
					<span className="font-medium">
						{d.updates.length} package{d.updates.length === 1 ? '' : 's'}
					</span>
					<span className="text-muted-foreground">
						{' '}
						{d.updates.length === 1 ? 'has' : 'have'} an update available.
					</span>
					{updateAllProgress && (
						<span className="text-muted-foreground">
							{' '}
							Updating {updateAllProgress.current || '…'} ({updateAllProgress.done}/
							{updateAllProgress.total})…
						</span>
					)}
				</Banner>
			)}
			{updateFailures.length > 0 && (
				<Banner
					tone="danger"
					icon={<AlertTriangle />}
					className="px-6 py-2.5"
					onDismiss={() => setUpdateFailures([])}
				>
					<span className="font-medium">
						{updateFailures.length} update{updateFailures.length === 1 ? '' : 's'} failed
					</span>
					<span className="text-muted-foreground">
						{' '}
						— {updateFailures.map((f) => `${f.name}: ${f.error}`).join(' · ')}
					</span>
				</Banner>
			)}
			<div className="flex-1 overflow-y-auto px-6 py-5">
				{d.error && (
					<FeedbackState
						variant="error"
						fill
						heading="Kernel error"
						body={d.error}
						className="mb-4 min-h-0"
					/>
				)}
				{d.isLoading && !d.installed.length && (
					<LoadingState
						data-state="ngwa-store-loading"
						fill
						heading={filter === 'store' ? 'Fetching the index' : 'Loading kernel status…'}
					/>
				)}
				{filter === 'store' && d.registryError && (
					<OfflineState
						data-state="ngwa-store-offline"
						fill
						heading="Registry unreachable"
						body="Everything installed still runs. Only browsing and installing new packages needs the network."
						action={{ label: 'Retry', onClick: d.retryRegistry }}
					/>
				)}
				{!(filter === 'store' && d.registryError) && !d.isLoading && !visible.length && (
					<FeedbackState
						variant="empty"
						fill
						heading={
							query
								? `No packages match "${query}".`
								: filter === 'updates'
									? 'Nothing to update.'
									: filter === 'review'
										? 'Nothing needs review.'
										: 'No packages installed.'
						}
					/>
				)}
				<div className="space-y-6">
					{Object.entries(groups).map(([label, rows]) => (
						<PkgGroup
							key={label}
							label={label}
							rows={rows}
							onOpen={(row) => openLoupe(row, 'overview')}
							onInstall={(row) => {
								setInstallPkg(row);
								setInstallOpen(true);
							}}
							onUpdate={(row) => {
								setInstallPkg(row);
								setInstallOpen(true);
							}}
							onReviewTrust={(row) => openLoupe(row, 'trust')}
						/>
					))}
				</div>
			</div>
			<PkgLoupe
				row={loupePkg}
				tab={loupeTab}
				open={!!loupePkg}
				onOpenChange={(open) => !open && setLoupePkg(null)}
				onInstall={(row) => {
					setInstallPkg(row);
					setInstallOpen(true);
				}}
				onUpdate={(row) => {
					setLoupePkg(null);
					setInstallPkg(row);
					setInstallOpen(true);
				}}
			/>
			<PkgInstallSheet
				open={installOpen}
				onOpenChange={setInstallOpen}
				pkg={installPkg}
				defaultTab={installTab}
			/>
		</div>
	);
}
