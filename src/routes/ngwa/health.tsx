// /ngwa/health — Ngwa Health Dashboard (WP-16 / WP-16a / locked D-02).
//
// Mounts the five health panels. `?section=` scrolls to and focuses a panel;
// the three retired settings pages redirect here with it.

import { createFileRoute, useNavigate } from '@tanstack/react-router';
import { z } from 'zod';
import { useNgwaSnapshot } from '@/lib/ngwa/use-ngwa-snapshot';
import { NgwaHealthSurface } from '@/shell/ngwa/ngwa-health-surface';
import { NgwaTabs } from '@/shell/ngwa/ngwa-tabs';
import '@/shell/ngwa/ngwa.css';

const healthSearchSchema = z.object({
	section: z.enum(['violations', 'sidecars', 'cron', 'data', 'engines']).optional(),
});

function NgwaHealthPage() {
	const { section } = Route.useSearch();
	const navigate = useNavigate();
	const { items, snapshot, unreadableSources, isLoading, error } = useNgwaSnapshot();

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
				onOpenStore={() => void navigate({ to: '/ngwa/store', search: { kind: 'engine' } })}
			/>
		</div>
	);
}

export const Route = createFileRoute('/ngwa/health')({
	component: NgwaHealthPage,
	validateSearch: healthSearchSchema,
});
