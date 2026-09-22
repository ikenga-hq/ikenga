// /ngwa/create — Ngwa In-Shell Scaffolding Surface (WP-24 / locked D-02).
//
// Mounts the 12-kind adaptive 4-question interview with live tri-pane preview.

import { createFileRoute } from '@tanstack/react-router';
import { z } from 'zod';
import { useNgwaSnapshot } from '@/lib/ngwa/use-ngwa-snapshot';
import { NgwaCreateSurface } from '@/shell/ngwa/ngwa-create-surface';
import { NgwaTabs } from '@/shell/ngwa/ngwa-tabs';
import '@/shell/ngwa/ngwa.css';

const createSearchSchema = z.object({
	kind: z.string().optional(),
	scope: z.string().optional(),
});

function NgwaCreatePage() {
	const search = Route.useSearch();
	const { items } = useNgwaSnapshot();

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			<NgwaTabs activeTab="create" installedCount={items.length} />
			<NgwaCreateSurface
				initialKind={search.kind}
				initialScope={search.scope}
			/>
		</div>
	);
}

export const Route = createFileRoute('/ngwa/create')({
	component: NgwaCreatePage,
	validateSearch: createSearchSchema,
});
