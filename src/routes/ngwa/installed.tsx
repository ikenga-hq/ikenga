// /ngwa/installed — Ngwa equipment catalogue (WP-10).
//
// Mounts the in-route NgwaFacetBar alongside NgwaSurface wrapped in .legacy-ngwa.

import { createFileRoute } from '@tanstack/react-router';
import { useQuery } from '@tanstack/react-query';
import { useMemo } from 'react';
import { z } from 'zod';

import { openExternalUrl } from '@/lib/transport';
import { claudeConfigQueryOptions, useClaudeConfigWatch } from '@/lib/queries/claude-config';
import { useShellStore } from '@/lib/shell/shell-store';
import {
	NgwaSurface,
	projectIdForRoot,
	type NgwaKindId,
	type NgwaScopeId,
	type NgwaSurfaceId,
	type NgwaSystemId,
} from '@/shell/claude-config/ngwa-surface';
import type { ClaudeStoreScope } from '@/lib/tauri-cmd';
import { NgwaFacetBar, type NgwaSearchParams } from './-facet-bar';

import '@/shell/claude-config/claude-config.css';

const ngwaSearchSchema = z.object({
	surface: z
		.enum(['browse', 'registry', 'store', 'graph', 'map', 'life', 'health', 'flow'])
		.optional()
		.catch(undefined),
	scope: z.string().optional(),
	kind: z.enum(['skills', 'agents', 'commands', 'hooks', 'mcps']).optional().catch(undefined),
	sys: z.string().optional(),
	install: z.string().optional(),
});

const SYSTEM_IDS: readonly NgwaSystemId[] = ['claude', 'gemini', 'codex'];

function parseSystems(raw: string | undefined): NgwaSystemId[] {
	if (!raw) return [];
	return raw
		.split(',')
		.map((s) => s.trim())
		.filter((s): s is NgwaSystemId => (SYSTEM_IDS as readonly string[]).includes(s));
}

function NgwaInstalledPage() {
	const activeProject = useShellStore((s) => s.activeProject);
	const projectRoots = useMemo(() => {
		const list: string[] = [];
		if (activeProject?.root_path) list.push(activeProject.root_path);
		for (const r of activeProject?.extra_roots ?? []) {
			if (!list.includes(r)) list.push(r);
		}
		return list;
	}, [activeProject?.root_path, activeProject?.extra_roots]);
	const projects = useShellStore((s) => s.projects);
	const watchEnabled = useShellStore((s) => s.claudeWatchEnabled);

	const query = useQuery(claudeConfigQueryOptions(projectRoots));
	useClaudeConfigWatch(projectRoots, watchEnabled);

	function handleOpenInEditor(path: string) {
		void openExternalUrl(path);
	}

	const search = Route.useSearch();
	const ngwaSurface: NgwaSurfaceId = search.surface ?? 'browse';
	const ngwaScope: NgwaScopeId = (search.scope as NgwaScopeId) ?? 'all';
	const ngwaKind: NgwaKindId = search.kind ?? 'skills';
	const ngwaSystems: NgwaSystemId[] = useMemo(() => parseSystems(search.sys), [search.sys]);

	const projectScopes = useMemo(
		() =>
			projectRoots.map((root) => {
				const basename = root.split('/').filter(Boolean).pop() ?? 'project';
				const id = projectIdForRoot(projects, root) ?? basename;
				return { key: `project:${id}` as ClaudeStoreScope, label: basename };
			}),
		[projectRoots, projects]
	);

	return (
		<div className="legacy-ngwa flex h-full flex-col bg-background text-foreground">
			<NgwaFacetBar search={search as NgwaSearchParams} />
			<div className="flex-1 min-h-0 overflow-y-auto">
				<NgwaSurface
					config={query.data ?? null}
					isLoading={query.isLoading}
					error={query.error ? String(query.error) : null}
					surface={ngwaSurface}
					scope={ngwaScope}
					kind={ngwaKind}
					systems={ngwaSystems}
					onEdit={handleOpenInEditor}
					projectScopes={projectScopes}
					projects={projects}
				/>
			</div>
		</div>
	);
}

export const Route = createFileRoute('/ngwa/installed')({
	component: NgwaInstalledPage,
	validateSearch: ngwaSearchSchema,
});
