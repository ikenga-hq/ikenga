import { Outlet, createFileRoute, useLocation } from '@tanstack/react-router';
import { useQuery } from '@tanstack/react-query';
import { useMemo } from 'react';
import { z } from 'zod';
import { openExternalUrl } from '@/lib/transport/shims';

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

import '@/shell/claude-config/claude-config.css';

// Ngwa deep-link params (threaded by ngwa-mode.tsx sidebar). All optional — a
// bare `/claude` is the Ngwa Browse landing (surface=browse, scope=all,
// kind=skills).
const ngwaSearchSchema = z.object({
	surface: z
		.enum(['browse', 'registry', 'store', 'graph', 'map', 'life', 'health', 'flow'])
		.optional()
		.catch(undefined),
	// 'all' | 'personal' | `project:<id>` (one per scanned project root).
	scope: z.string().optional(),
	// `store` is no longer a kind (it graduated to its own surface) — `.catch`
	// degrades a stale `?kind=store` bookmark to the default kind instead of
	// throwing a validateSearch error.
	kind: z.enum(['skills', 'agents', 'commands', 'hooks', 'mcps']).optional().catch(undefined),
	// SYSTEM facet (WP-20) — multi-select, comma-separated engine ids
	// (`claude,gemini,codex`). Absent/empty ⇒ all present engines on.
	sys: z.string().optional(),
});

const SYSTEM_IDS: readonly NgwaSystemId[] = ['claude', 'gemini', 'codex'];

function parseSystems(raw: string | undefined): NgwaSystemId[] {
	if (!raw) return [];
	return raw
		.split(',')
		.map((s) => s.trim())
		.filter((s): s is NgwaSystemId => (SYSTEM_IDS as readonly string[]).includes(s));
}

export const Route = createFileRoute('/claude')({
	component: ClaudeLayout,
	validateSearch: ngwaSearchSchema,
});

function ClaudeLayout() {
	const projectRoots = useShellStore((s) => s.claudeProjectRoots);
	const projects = useShellStore((s) => s.projects);
	const watchEnabled = useShellStore((s) => s.claudeWatchEnabled);

	const query = useQuery(claudeConfigQueryOptions(projectRoots));
	useClaudeConfigWatch(projectRoots, watchEnabled);

	function handleOpenInEditor(path: string) {
		void openExternalUrl(path);
	}

	const path = useLocation({ select: (l) => l.pathname });
	const isNgwaIndex = path === '/claude';

	// Ngwa surface params. The bare `/claude` path is the Ngwa Manage surface
	// (Browse/Registry + write actions). Child paths (only `/claude/runtime-mcps`
	// survives) render bare into the Outlet.
	const search = Route.useSearch();
	const ngwaSurface: NgwaSurfaceId = search.surface ?? 'browse';
	const ngwaScope: NgwaScopeId = (search.scope as NgwaScopeId) ?? 'all';
	const ngwaKind: NgwaKindId = search.kind ?? 'skills';
	const ngwaSystems: NgwaSystemId[] = useMemo(() => parseSystems(search.sys), [search.sys]);

	// Derive the move/copy/install destination scopes from the user's project
	// roots, mapping each root onto the store-scope grammar (`project:<id>`). The
	// key MUST be the real DB project id (the slug the Rust store resolves via
	// `get_project`); the basename is only a display label and a last-resort
	// fallback for roots with no matching project row.
	const projectScopes = useMemo(
		() =>
			projectRoots.map((root) => {
				const basename = root.split('/').filter(Boolean).pop() ?? 'project';
				const id = projectIdForRoot(projects, root) ?? basename;
				return { key: `project:${id}` as ClaudeStoreScope, label: basename };
			}),
		[projectRoots, projects]
	);

	if (isNgwaIndex) {
		return (
			<div className="flex h-full flex-col">
				<div className="flex-1 min-h-0">
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

	return (
		<div className="flex h-full flex-col">
			<Outlet />
		</div>
	);
}
