// /ngwa/store — Ngwa Package Store (WP-15 / locked D-02).
//
// Mounts NgwaStoreSurface with enriched registry catalog and updates banner.

import { useCallback } from 'react';
import { createFileRoute } from '@tanstack/react-router';
import { z } from 'zod';
import { useNgwaSnapshot } from '@/lib/ngwa/use-ngwa-snapshot';
import { useStoreInstall } from '@/lib/ngwa/use-store-install';
import { fetchPkgVersionForStore } from '@/lib/registry/client';
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
});

function NgwaStorePage() {
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
	// Install / update through the shared signed-registry plan path.
	const { install, update, updateAll } = useStoreInstall();
	const activeProjectName = useShellStore((s) => {
		const p = s.projects.find((x) => x.id === s.activeProjectId);
		return p?.display_name || p?.id;
	});

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			<NgwaTabs activeTab="store" installedCount={items.length} />
			<NgwaStoreSurface
				catalog={storeCatalog}
				isLoading={isLoading}
				error={error}
				onRetry={refetch}
				loadDetail={indexUrl ? loadDetail : undefined}
				activeProjectName={activeProjectName}
				onInstall={install}
				onUpdate={update}
				onUpdateAll={updateAll}
			/>
		</div>
	);
}

export const Route = createFileRoute('/ngwa/store')({
	component: NgwaStorePage,
	validateSearch: searchSchema,
});
