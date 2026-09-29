// /ngwa/scopes — Ngwa Scopes Matrix (WP-16 / WP-16a / locked D-02).
//
// Mounts the matrix surface and wires every cell action to its real command:
// Claude primitives through the claude-config mutation hooks
// (`claudePrimitive*`, scope `'workspace'` = personal, `project:<id>` = a
// project), pkgs through `pkgSetEnabled` / `pkgUninstall`. Every mutation
// invalidates the Ngwa snapshot so the matrix re-reads the disk. The writers
// live in `lib/ngwa/use-ngwa-actions` (shared with Installed + item detail).

import { createFileRoute, useNavigate } from '@tanstack/react-router';
import { useIsFetching } from '@tanstack/react-query';
import { z } from 'zod';
import { ngwaSnapshotQueryKey, useNgwaSnapshot } from '@/lib/ngwa/use-ngwa-snapshot';
import { useNgwaScopeActions, useNgwaScopeColumns } from '@/lib/ngwa/use-ngwa-actions';
import { NgwaScopesSurface } from '@/shell/ngwa/ngwa-scopes-surface';
import { NgwaTabs } from '@/shell/ngwa/ngwa-tabs';
import '@/shell/ngwa/ngwa.css';

const scopesSearchSchema = z.object({
	kind: z.string().optional(),
	/** `personal` or a project id — that column is placed first and highlighted. */
	scope: z.string().optional(),
	search: z.string().optional(),
});

function NgwaScopesPage() {
	const search = Route.useSearch();
	const navigate = useNavigate();
	const { items, unreadableSources, isLoading, error } = useNgwaSnapshot();
	const refreshing = useIsFetching({ queryKey: ngwaSnapshotQueryKey }) > 0;
	// The writers, the scope columns and the resolved home are shared with the
	// Installed tab and the item detail (`lib/ngwa/use-ngwa-actions`).
	const { scopes, homeDir } = useNgwaScopeColumns();
	const actions = useNgwaScopeActions();

	const focusScope =
		search.scope === undefined
			? undefined
			: search.scope === 'personal'
				? 'personal'
				: `project:${search.scope}`;

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			<NgwaTabs activeTab="scopes" installedCount={items.length} />
			<NgwaScopesSurface
				items={items}
				isLoading={isLoading}
				error={error}
				unreadableSources={unreadableSources}
				scopes={scopes}
				homeDir={homeDir}
				actions={actions}
				kind={search.kind ?? '*'}
				onKindChange={(kind) =>
					void navigate({
						to: '/ngwa/scopes',
						search: (prev) => ({ ...prev, kind: kind === '*' ? undefined : kind }),
						replace: true,
					})
				}
				search={search.search}
				focusScope={focusScope}
				refreshing={refreshing}
			/>
		</div>
	);
}

export const Route = createFileRoute('/ngwa/scopes')({
	component: NgwaScopesPage,
	validateSearch: scopesSearchSchema,
});
