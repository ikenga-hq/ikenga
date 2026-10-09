// /ngwa/health — Ngwa Health Dashboard (WP-16 / WP-16a / locked D-02).
//
// Mounts the six D-02 health panels. `?section=` scrolls to and focuses a panel;
// the three retired settings pages redirect here with it.

import { createFileRoute, useNavigate } from '@tanstack/react-router';
import { z } from 'zod';
import { registryNameMatches } from '@/lib/ngwa/broken-pkgs';
import { useNgwaSnapshot } from '@/lib/ngwa/use-ngwa-snapshot';
import { useRegistryIndex } from '@/lib/registry/use-registry';
import { NgwaHealthSurface } from '@/shell/ngwa/ngwa-health-surface';
import { NgwaTabs } from '@/shell/ngwa/ngwa-tabs';
import '@/shell/ngwa/ngwa.css';

const healthSearchSchema = z.object({
	section: z
		.enum(['connection', 'violations', 'sidecars', 'cron', 'data', 'trust', 'engines'])
		.optional(),
});

function NgwaHealthPage() {
	const { section } = Route.useSearch();
	const navigate = useNavigate();
	const { items, snapshot, unreadableSources, isLoading, error } = useNgwaSnapshot();
	const registryPkgs = useRegistryIndex().data?.index.pkgs ?? [];

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			<NgwaTabs activeTab="health" installedCount={items.length} />
			<NgwaHealthSurface
				items={items}
				snapshot={snapshot}
				isLoading={isLoading}
				error={error}
				unreadableSources={unreadableSources}
				section={section}
				onOpenBackup={() => void navigate({ to: '/settings/backup' })}
				onOpenServer={() => void navigate({ to: '/settings/about', hash: 'server-health' })}
				onOpenStore={() => void navigate({ to: '/ngwa/store', search: { kind: 'engine' } })}
				canReinstall={(pkgId) => registryPkgs.some((e) => registryNameMatches(e.name, pkgId))}
				// D-02 "Reinstall from registry": the pkg's Store sheet runs the
				// shared registry install path, behind its consent step.
				onReinstall={(pkgId) => void navigate({ to: '/ngwa/store', search: { pkg: pkgId } })}
			/>
		</div>
	);
}

export const Route = createFileRoute('/ngwa/health')({
	component: NgwaHealthPage,
	validateSearch: healthSearchSchema,
});
