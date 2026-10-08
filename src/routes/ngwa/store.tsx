// /ngwa/store — Ngwa Package Store (WP-15 / locked D-02, R57 addendum).
//
// Mounts NgwaStoreSurface with enriched registry catalog and updates banner,
// plus (R57) the signed primitive catalog's git / npx rows, Add from URL, and
// the pinned catalog auto-update sweep on mount (Q3).

import { useCallback, useMemo } from 'react';
import { createFileRoute, useNavigate } from '@tanstack/react-router';
import { z } from 'zod';
import { installUnavailableReason } from '@/lib/desktop-only';
import { markBrokenEntries, registryNameMatches, useBrokenPkgs } from '@/lib/ngwa/broken-pkgs';
import { mergeCatalogIntoStore } from '@/lib/ngwa/enrichment';
import { useNgwaSnapshot } from '@/lib/ngwa/use-ngwa-snapshot';
import { useStoreInstall } from '@/lib/ngwa/use-store-install';
import { useUpdateApprovals } from '@/lib/ngwa/use-update-approvals';
import { useVaultEntries } from '@/lib/ngwa/use-vault-entries';
import { useObaAutoUpdateOnMount } from '@/lib/queries/claude-config';
import { fetchPkgVersionForStore } from '@/lib/registry/client';
import { catalogPins, usePrimitiveCatalogResult } from '@/lib/registry/primitives';
import { useRegistryIndex } from '@/lib/registry/use-registry';
import { useShellStore } from '@/lib/shell/shell-store';
import { NgwaStoreSurface, type StoreDetailLoader } from '@/shell/ngwa/ngwa-store-surface';
import { NgwaTabs } from '@/shell/ngwa/ngwa-tabs';
import '@/shell/ngwa/ngwa.css';

const searchSchema = z.object({
	filter: z.string().optional(),
	install: z.string().optional(),
	surface: z.string().optional(),
	scope: z.string().optional(),
	kind: z.string().optional(),
	sys: z.string().optional(),
	search: z.string().optional(),
	/** `ngwa.add-from-url`: open the Add from URL sheet. */
	addurl: z.union([z.string(), z.number(), z.boolean()]).optional(),
	/** A pkg id (or registry name): open its sheet. Health's "Reinstall from
	 *  registry" lands here so the reinstall goes through the sheet's consent. */
	pkg: z.string().optional(),
});

function NgwaStorePage() {
	const { addurl, pkg } = Route.useSearch();
	const navigate = useNavigate();
	const { items, storeCatalog, isLoading, error, refetch } = useNgwaSnapshot();
	// The install sheet lazily reads the selected pkg's detail file, relative
	// to the verified index URL (same query the snapshot hook already holds).
	const indexUrl = useRegistryIndex().data?.indexUrl;
	const loadDetail = useCallback<StoreDetailLoader>(
		(entry, signal) => {
			if (!indexUrl) throw new Error('Registry index not loaded');
			return fetchPkgVersionForStore(indexUrl, entry.registryEntry, entry.latestVersion, signal);
		},
		[indexUrl]
	);
	// Install / update through the shared signed-registry plan path, and (R57)
	// the vault path for git / npx primitives.
	const store = useStoreInstall();
	// An update held back for new permissions opens the updater's trust
	// review modal (mounted once, below); approve installs it.
	const approvals = useUpdateApprovals({ update: store.update });
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const activeProjectName = useShellStore((s) => {
		const p = s.projects.find((x) => x.id === s.activeProjectId);
		return p?.display_name || p?.id;
	});

	// R57 · the signed catalog. A verify failure is an error, never the seed.
	const catalogQuery = usePrimitiveCatalogResult();
	const catalogEntries = catalogQuery.data?.entries ?? [];
	const vault = useVaultEntries();
	// On disk but failed to register → the row reads Reinstall (install health).
	const broken = useBrokenPkgs();
	const { registry, primitives } = useMemo(() => {
		const merged = mergeCatalogIntoStore(storeCatalog, catalogEntries, vault.entries);
		return { ...merged, registry: markBrokenEntries(merged.registry, broken) };
	}, [storeCatalog, catalogEntries, vault.entries, broken]);
	const initialSelectedId = useMemo(
		() => (pkg ? (storeCatalog.find((e) => registryNameMatches(e.name, pkg))?.id ?? pkg) : null),
		[pkg, storeCatalog]
	);
	const catalogStatus = catalogQuery.isLoading
		? 'loading'
		: catalogQuery.error
			? 'error'
			: catalogQuery.data?.verified
				? 'verified'
				: 'seed';

	// Q3: one auto-update sweep per Store mount, once the catalog is read, with
	// its pins — a pinned catalog install moves only to its catalog pin.
	const pins = useMemo(() => catalogPins(catalogEntries), [catalogEntries]);
	useObaAutoUpdateOnMount(catalogQuery.isSuccess, pins);

	const ctx = { catalog: catalogEntries, vault: vault.entries };
	// Gap audit rank 3: the daemon serves no install or update yet. Leaving the
	// handlers off makes the surface disable Install / Update / Update all
	// before the click (with its NOT_AVAILABLE_ON_SERVER_YET reason) instead of
	// letting a click end in a raw error. Drop this gate once they are served.
	const installReason = installUnavailableReason();
	const installBlocked = installReason !== false;

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			<NgwaTabs activeTab="store" installedCount={items.length} />
			<NgwaStoreSurface
				catalog={registry}
				disabledReason={installReason || undefined}
				isLoading={isLoading}
				error={error}
				onRetry={refetch}
				loadDetail={indexUrl ? loadDetail : undefined}
				activeProjectName={activeProjectName}
				activeProjectId={activeProjectId}
				onInstall={installBlocked ? undefined : store.install}
				onUpdate={installBlocked ? undefined : store.update}
				onUpdateAll={installBlocked ? undefined : store.updateAll}
				updateApprovals={approvals}
				primitives={primitives}
				catalogEntries={catalogEntries}
				vault={vault.entries}
				catalogStatus={catalogStatus}
				catalogError={catalogQuery.error ? (catalogQuery.error as Error).message : null}
				onRecheckCatalog={() => void catalogQuery.refetch()}
				onInstallPrimitive={
					installBlocked
						? undefined
						: (row, scope, onStage) => store.installPrimitive(row, scope, { ...ctx, onStage })
				}
				onUpdatePrimitive={installBlocked ? undefined : store.updatePrimitive}
				onResolveSource={store.resolveSource}
				onInstallResolved={
					installBlocked
						? undefined
						: (resolved, scope, onStage) =>
								store.installResolved(resolved, scope, { ...ctx, onStage })
				}
				onOpenInstalled={(name) =>
					void navigate({ to: '/ngwa/installed', search: { search: name } })
				}
				initialAddUrl={addurl !== undefined}
				// Keyed so a late-loading catalog (or a new ?pkg=) re-opens the sheet.
				key={initialSelectedId ?? undefined}
				initialSelectedId={initialSelectedId}
			/>
			{approvals.element}
		</div>
	);
}

export const Route = createFileRoute('/ngwa/store')({
	component: NgwaStorePage,
	validateSearch: searchSchema,
});
