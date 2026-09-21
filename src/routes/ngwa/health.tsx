// /ngwa/health — Ngwa Health Dashboard (WP-16 / locked D-02).
//
// Mounts the unified health grid (Violations, Sidecars, Cron, Data, Engines).

import { createFileRoute } from '@tanstack/react-router';
import { z } from 'zod';
import { useNgwaSnapshot } from '@/lib/ngwa/use-ngwa-snapshot';
import { NgwaHealthSurface } from '@/shell/ngwa/ngwa-health-surface';
import { NgwaTabs } from '@/shell/ngwa/ngwa-tabs';
import '@/shell/ngwa/ngwa.css';

const healthSearchSchema = z.object({
	section: z.enum(['violations', 'sidecars', 'cron', 'data', 'engines']).optional(),
});

function NgwaHealthPage() {
	const { items, isLoading, error } = useNgwaSnapshot();

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			<NgwaTabs activeTab="health" installedCount={items.length} />
			<NgwaHealthSurface
				items={items}
				isLoading={isLoading}
				error={error}
			/>
		</div>
	);
}

export const Route = createFileRoute('/ngwa/health')({
	component: NgwaHealthPage,
	validateSearch: healthSearchSchema,
});
