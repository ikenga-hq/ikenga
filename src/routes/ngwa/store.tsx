// /ngwa/store — Ngwa Package Store (WP-15 / locked D-02).
//
// Mounts NgwaStoreSurface with enriched registry catalog and updates banner.

import { createFileRoute } from '@tanstack/react-router';
import { z } from 'zod';
import { useNgwaSnapshot } from '@/lib/ngwa/use-ngwa-snapshot';
import { NgwaStoreSurface } from '@/shell/ngwa/ngwa-store-surface';
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
	const { items, storeCatalog, isLoading, error } = useNgwaSnapshot();

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			<NgwaTabs activeTab="store" installedCount={items.length} />
			<NgwaStoreSurface
				catalog={storeCatalog}
				isLoading={isLoading}
				error={error}
			/>
		</div>
	);
}

export const Route = createFileRoute('/ngwa/store')({
	component: NgwaStorePage,
	validateSearch: searchSchema,
});
