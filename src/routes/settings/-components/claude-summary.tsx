// Slim Claude Code summary for Settings → Integrations. The previous
// Phase-3 quarantined `ClaudeConfigSectionBody` duplicated the project-roots
// editor that lives in Settings → Storage and the watch toggle that's now
// part of the /claude browser's own controls. After the legacy-sections
// split, this surface is just a one-line status + a deep link to /claude,
// which is the canonical viewer.

import { useQuery } from '@tanstack/react-query';
import { Link } from '@tanstack/react-router';
import { ExternalLink, FolderTree, Layers } from 'lucide-react';

import { Button } from '@/components/ui/button';
import { claudeConfigQueryOptions } from '@/lib/queries/claude-config';
import { useShellStore } from '@/lib/shell/shell-store';

export function ClaudeSummarySectionBody() {
	const activeProject = useShellStore((s) => s.activeProject);
	const projectRoots = activeProject?.root_path
		? [activeProject.root_path, ...activeProject.extra_roots.filter((r) => r !== activeProject.root_path)]
		: activeProject?.extra_roots ?? [];
	const query = useQuery(claudeConfigQueryOptions(projectRoots));

	const total = query.data
		? query.data.agents.length +
			query.data.skills.length +
			query.data.commands.length +
			query.data.hooks.length +
			query.data.mcps.length
		: null;

	return (
		<div className="flex items-center justify-between gap-3 px-4 py-3">
			<div className="min-w-0 flex-1 space-y-1">
				<div className="flex items-center gap-2 text-sm">
					<FolderTree className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
					<span className="font-medium text-foreground">Claude Code config</span>
					{total != null && (
						<span className="font-mono text-[11px] text-muted-foreground">{total} entries</span>
					)}
				</div>
				<p className="text-xs text-muted-foreground">
					Agents, skills, commands, hooks and MCP servers discovered under each project root and
					your personal <code>~/.claude/</code>. Manage project roots in{' '}
					<Link
						to="/settings/storage"
						className="underline decoration-dotted underline-offset-2 hover:text-foreground"
					>
						Settings → Storage
					</Link>
					.
				</p>
				<p className="text-xs text-muted-foreground">
					<Link
						to="/claude"
						className="inline-flex items-center gap-1 underline decoration-dotted underline-offset-2 hover:text-foreground"
					>
						<Layers className="h-3 w-3" />
						Layered View
					</Link>{' '}
					— pkg-aware 4-tier discovery with conflict surfacing and pin controls.
				</p>
			</div>
			<Button asChild variant="outline" size="sm">
				<Link to="/claude">
					<ExternalLink className="mr-1 h-3.5 w-3.5" />
					Open /claude
				</Link>
			</Button>
		</div>
	);
}
