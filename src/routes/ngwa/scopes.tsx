// /ngwa/scopes — Ngwa Scopes Matrix (WP-16 / WP-16a / locked D-02).
//
// Mounts the matrix surface and wires every cell action to its real command:
// Claude primitives through the claude-config mutation hooks
// (`claudePrimitive*`, scope `'workspace'` = personal, `project:<id>` = a
// project), pkgs through `pkgSetEnabled` / `pkgUninstall`. Every mutation
// invalidates the Ngwa snapshot so the matrix re-reads the disk.

import { useCallback, useMemo } from 'react';
import { createFileRoute, useNavigate } from '@tanstack/react-router';
import { useQueryClient } from '@tanstack/react-query';
import { z } from 'zod';
import { ngwaSnapshotQueryKey, useNgwaSnapshot } from '@/lib/ngwa/use-ngwa-snapshot';
import {
	useCopyPrimitive,
	useDisablePrimitive,
	useDisablePrimitiveFor,
	useEnablePrimitive,
	useEnablePrimitiveFor,
	useMovePrimitive,
	useRemovePrimitive,
} from '@/lib/queries/claude-config';
import { useShellStore } from '@/lib/shell/shell-store';
import { pkgSetEnabled, pkgUninstall } from '@/lib/tauri-cmd';
import {
	NgwaScopesSurface,
	type NgwaScopeActions,
	type ScopeColumn,
} from '@/shell/ngwa/ngwa-scopes-surface';
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
	const qc = useQueryClient();
	const { items, unreadableSources, isLoading, error } = useNgwaSnapshot();
	const projects = useShellStore((s) => s.projects);
	const activeProjectId = useShellStore((s) => s.activeProjectId);

	const scopes = useMemo<ScopeColumn[]>(() => {
		const cols: ScopeColumn[] = [
			{ key: 'personal', label: 'Personal', sub: '~/.claude', active: false },
		];
		const live = projects
			.filter((p) => p.archived_at === null && p.root_path)
			.slice()
			.sort((a, b) => a.position - b.position);
		for (const p of live) {
			cols.push({
				key: `project:${p.id}`,
				label: p.display_name || p.id,
				sub: '.claude',
				active: p.id === activeProjectId,
			});
		}
		return cols;
	}, [projects, activeProjectId]);

	const enable = useEnablePrimitive();
	const disable = useDisablePrimitive();
	const copy = useCopyPrimitive();
	const move = useMovePrimitive();
	const remove = useRemovePrimitive();
	const enableFor = useEnablePrimitiveFor();
	const disableFor = useDisablePrimitiveFor();

	const refresh = useCallback(
		() => qc.invalidateQueries({ queryKey: ngwaSnapshotQueryKey }),
		[qc]
	);
	/** Run a mutation, then invalidate the snapshot whether it worked or not —
	 *  a partial write still changed the disk. */
	const after = useCallback(
		async <T,>(p: Promise<T>): Promise<T> => {
			try {
				return await p;
			} finally {
				void refresh();
			}
		},
		[refresh]
	);

	const actions = useMemo<NgwaScopeActions>(
		() => ({
			enable: (kind, name, scope) => after(enable.mutateAsync({ kind, name, scope })),
			disable: (kind, name, scope) => after(disable.mutateAsync({ kind, name, scope })),
			copy: (kind, name, fromScope, toScope) =>
				after(copy.mutateAsync({ kind, name, fromScope, toScope })),
			move: (kind, name, fromScope, toScope) =>
				after(move.mutateAsync({ kind, name, fromScope, toScope })),
			remove: (kind, name, scope) => after(remove.mutateAsync({ kind, name, scope })),
			enableFor: (engine, kind, name, scope) =>
				after(enableFor.mutateAsync({ engine, kind, name, scope })),
			disableFor: (engine, kind, name, scope) =>
				after(disableFor.mutateAsync({ engine, kind, name, scope })),
			pkgSetEnabled: (pkgId, enabled) => after(pkgSetEnabled(pkgId, enabled)),
			pkgUninstall: (pkgId) => after(pkgUninstall(pkgId)),
			openStore: () => void navigate({ to: '/ngwa/store', search: { kind: 'engine' } }),
		}),
		[after, enable, disable, copy, move, remove, enableFor, disableFor, navigate]
	);

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
			/>
		</div>
	);
}

export const Route = createFileRoute('/ngwa/scopes')({
	component: NgwaScopesPage,
	validateSearch: scopesSearchSchema,
});
