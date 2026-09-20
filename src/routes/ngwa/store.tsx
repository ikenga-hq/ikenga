// /ngwa/store — Ngwa Package Store (WP-10).
//
// Mounts PkgsSurface with store filter alongside NgwaFacetBar.

import { createFileRoute } from '@tanstack/react-router';
import { z } from 'zod';
import { PkgsSurface } from '@/components/pkg/v2/pkgs-surface';
import { NgwaFacetBar, type NgwaSearchParams } from './-facet-bar';

const searchSchema = z.object({
	filter: z.enum(['all', 'installed', 'updates', 'store', 'review', 'disabled']).optional(),
	install: z.enum(['manifest-url', 'local-path', 'registry']).optional(),
	surface: z.any().optional(),
	scope: z.string().optional(),
	kind: z.any().optional(),
	sys: z.string().optional(),
});

function NgwaStorePage() {
	const search = Route.useSearch();
	return (
		<div className="flex h-full flex-col bg-background text-foreground">
			<NgwaFacetBar search={{ ...search, surface: 'store' } as NgwaSearchParams} />
			<div className="flex-1 min-h-0 overflow-y-auto">
				<PkgsSurface initialFilter={search.filter ?? 'store'} initialInstallTab={search.install} />
			</div>
		</div>
	);
}

export const Route = createFileRoute('/ngwa/store')({
	component: NgwaStorePage,
	validateSearch: searchSchema,
});
