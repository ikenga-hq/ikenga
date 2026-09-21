// /ngwa/scopes — Ngwa Scopes Matrix (WP-16 / locked D-02).
//
// Mounts the 3-axis matrix surface: Equipment × Scopes × Engines.

import { createFileRoute } from '@tanstack/react-router';
import { z } from 'zod';
import { useNgwaSnapshot } from '@/lib/ngwa/use-ngwa-snapshot';
import { useShellStore } from '@/lib/shell/shell-store';
import { NgwaScopesSurface } from '@/shell/ngwa/ngwa-scopes-surface';
import { NgwaTabs } from '@/shell/ngwa/ngwa-tabs';
import '@/shell/ngwa/ngwa.css';

const scopesSearchSchema = z.object({
	kind: z.string().optional(),
	scope: z.string().optional(),
	search: z.string().optional(),
});

function NgwaScopesPage() {
	const { items, isLoading, error } = useNgwaSnapshot();
	const activeProject = useShellStore((s) => s.activeProject);
	const activeProjectName =
		activeProject?.root_path?.split(/[\\/]/).filter(Boolean).pop() || 'project';

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			<NgwaTabs activeTab="scopes" installedCount={items.length} />
			<NgwaScopesSurface
				items={items}
				isLoading={isLoading}
				error={error}
				activeProjectName={activeProjectName}
			/>
		</div>
	);
}

export const Route = createFileRoute('/ngwa/scopes')({
	component: NgwaScopesPage,
	validateSearch: scopesSearchSchema,
});
