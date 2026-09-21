// /ngwa/installed — Ngwa equipment catalogue (WP-15 / locked D-02).
//
// Mounts the unified NgwaList with enriched items, unreadable sources, and facets.

import { createFileRoute } from '@tanstack/react-router';
import { z } from 'zod';
import { useNgwaSnapshot } from '@/lib/ngwa/use-ngwa-snapshot';
import { NgwaList } from '@/shell/ngwa/ngwa-list';
import { NgwaTabs } from '@/shell/ngwa/ngwa-tabs';
import '@/shell/ngwa/ngwa.css';

const ngwaSearchSchema = z.object({
	kind: z.string().optional(),
	scope: z.string().optional(),
	search: z.string().optional(),
	pkg: z.string().optional(),
	filter: z.string().optional(),
	install: z.string().optional(),
	surface: z.string().optional(),
	sys: z.string().optional(),
});

function NgwaInstalledPage() {
	const { items, unreadableSources, isLoading, error } = useNgwaSnapshot();

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			<NgwaTabs activeTab="installed" installedCount={items.length} />
			<NgwaList
				items={items}
				unreadableSources={unreadableSources}
				isLoading={isLoading}
				error={error}
			/>
		</div>
	);
}

export const Route = createFileRoute('/ngwa/installed')({
	component: NgwaInstalledPage,
	validateSearch: ngwaSearchSchema,
});
