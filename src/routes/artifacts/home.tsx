// /artifacts/home — the Studio home view.
//
// Single anchor page for the artifact-grid mode. Pulls together everything
// the new walker + recents already give us:
//   - Header with project + a big "+ New artifact" CTA
//   - Recents shelf (folders the user has been working in)
//   - By-archetype tiles (counts from the walker, click to drill into a
//     filtered grid; archetypes with zero artifacts open the wizard
//     prefilled with that archetype)
//   - Drafts callout (in-progress artifacts: no version or 0.x)
//   - Recently modified list (last 10 .html files in the project)
//
// Reached via:
//   - The artifact-grid sidebar's Tools row "Home" button
//   - Direct nav to /artifacts/home

import { useMemo } from 'react';
import { createFileRoute, useNavigate } from '@tanstack/react-router';
import { useQuery } from '@tanstack/react-query';
import { open as openDialog } from '@/lib/transport/dialog-shim';
import * as Icons from 'lucide-react';
import { FileText, FolderOpen, Plus, Sparkles, Star } from 'lucide-react';

import { Button } from '@/components/ui/button';
import { cn } from '@/components/ui/utils';
import { usePaneStore } from '@/lib/panes/pane-store';
import {
	type ArtifactCatalog,
	type ArtifactCounts,
	projectArtifactsQueryOptions,
} from '@/lib/queries/project-artifacts';
import { useShellStore } from '@/lib/shell/shell-store';
import { openArtifactGrid } from '@/lib/shell/artifact-grid-recents';
import { ARCHETYPES, type Archetype } from '@/shell/artifact-wizard/archetypes';
import type { Project } from '@/lib/tauri-cmd';

export const Route = createFileRoute('/artifacts/home')({
	component: ArtifactsHomePage,
});

function ArtifactsHomePage() {
	const navigate = useNavigate();
	const activeProject = useShellStore(
		(s) => s.projects.find((p) => p.id === s.activeProjectId) ?? null
	);
	const catalogQuery = useQuery(projectArtifactsQueryOptions(activeProject?.root_path ?? null));

	function openWizard(archetype?: string) {
		void navigate({
			to: '/projects/new-artifact',
			search: archetype ? { archetype } : undefined,
		});
	}

	function openLoupe(path: string) {
		const ps = usePaneStore.getState();
		ps.addTab(ps.focusedId, { kind: 'artifact-studio', path, density: 'loupe' });
	}

	async function browseFolder() {
		if (!activeProject) return;
		try {
			const picked = await openDialog({ directory: true, multiple: false });
			if (typeof picked === 'string' && picked.length > 0) {
				await openArtifactGrid(activeProject.id, picked);
			}
		} catch (e) {
			console.error('[artifacts-home] folder-picker failed', e);
		}
	}

	if (!activeProject) {
		return (
			<NoProjectState onNavigateSettings={() => void navigate({ to: '/settings/projects' })} />
		);
	}

	return (
		<div className="flex h-full flex-col overflow-y-auto">
			<HeaderBar
				project={activeProject}
				onNew={() => openWizard()}
				onBrowse={() => void browseFolder()}
			/>

			<div className="mx-auto flex w-full max-w-5xl flex-col gap-6 px-6 py-6">
				<ByArchetypeRow
					counts={catalogQuery.data?.counts}
					loading={catalogQuery.isLoading}
					onPick={(slug, hasAny) => {
						if (hasAny) {
							void navigate({
								to: '/artifacts/by-kind/$kind',
								params: { kind: slug },
							});
						} else {
							openWizard(slug);
						}
					}}
				/>

				<RecentlyModified
					catalog={catalogQuery.data}
					loading={catalogQuery.isLoading}
					onOpen={openLoupe}
				/>
			</div>
		</div>
	);
}

// ─── Header bar ──────────────────────────────────────────────────────────

function HeaderBar({
	project,
	onNew,
	onBrowse,
}: {
	project: Project;
	onNew: () => void;
	onBrowse: () => void;
}) {
	return (
		<div className="flex shrink-0 items-center gap-3 border-b border-border px-6 py-3">
			<span
				aria-hidden
				className="inline-block h-3 w-3 shrink-0 rounded-full border border-border"
				style={{ background: project.color ?? '#7c7c7c' }}
			/>
			<div className="flex flex-col min-w-0">
				<span className="text-sm font-semibold text-foreground">{project.display_name}</span>
				{project.root_path && (
					<span className="truncate font-mono text-[10px] text-muted-foreground">
						{project.root_path}
					</span>
				)}
			</div>
			<div className="ml-auto flex items-center gap-2">
				<Button variant="outline" size="sm" onClick={onBrowse}>
					<FolderOpen className="mr-1 h-3.5 w-3.5" />
					Open folder
				</Button>
				<Button size="sm" onClick={onNew}>
					<Plus className="mr-1 h-3.5 w-3.5" />
					New artifact
					<span className="ml-2 font-mono text-[10px] opacity-70">⌘⇧N</span>
				</Button>
			</div>
		</div>
	);
}

// ─── By archetype ────────────────────────────────────────────────────────

function ByArchetypeRow({
	counts,
	loading,
	onPick,
}: {
	counts: ArtifactCounts | undefined;
	loading: boolean;
	onPick: (slug: string, hasAny: boolean) => void;
}) {
	return (
		<section className="flex flex-col gap-2">
			<SectionHeading label="By archetype" />
			<div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-4">
				{ARCHETYPES.map((a) => {
					const n = counts?.byKind[a.slug] ?? 0;
					return (
						<ArchetypeTile
							key={a.slug}
							archetype={a}
							count={n}
							loading={loading && !counts}
							onPick={() => onPick(a.slug, n > 0)}
						/>
					);
				})}
			</div>
		</section>
	);
}

function ArchetypeTile({
	archetype,
	count,
	loading,
	onPick,
}: {
	archetype: Archetype;
	count: number;
	loading: boolean;
	onPick: () => void;
}) {
	const Glyph = resolveGlyph(archetype.glyphName);
	return (
		<button
			type="button"
			onClick={onPick}
			className={cn(
				'flex flex-col items-start gap-1.5 rounded border border-border bg-card px-3 py-2.5 text-left transition-colors',
				'hover:border-foreground/40 hover:bg-accent'
			)}
		>
			<div className="flex w-full items-center gap-2">
				<Glyph className="h-4 w-4 text-muted-foreground" />
				<span className="flex-1 text-sm font-medium text-foreground">{archetype.label}</span>
				<span className="font-mono text-[11px] text-muted-foreground">{loading ? '…' : count}</span>
			</div>
			<p className="text-[10px] leading-snug text-muted-foreground">{archetype.description}</p>
		</button>
	);
}

// ─── Recently modified ───────────────────────────────────────────────────

function RecentlyModified({
	catalog,
	loading,
	onOpen,
}: {
	catalog: ArtifactCatalog | undefined;
	loading: boolean;
	onOpen: (path: string) => void;
}) {
	const rows = useMemo(() => {
		if (!catalog) return [];
		return [...catalog.rows].sort((a, b) => b.modified_at - a.modified_at).slice(0, 10);
	}, [catalog]);

	return (
		<section className="flex flex-col gap-2">
			<SectionHeading
				label="Recently modified"
				detail={catalog ? `${catalog.counts.all} total` : undefined}
			/>
			{loading && !catalog ? (
				<div className="rounded border border-dashed border-border px-3 py-3 text-xs text-muted-foreground">
					Loading…
				</div>
			) : rows.length === 0 ? (
				<div className="rounded border border-dashed border-border px-3 py-3 text-xs text-muted-foreground">
					No <code>.html</code> artifacts in this project yet. Use{' '}
					<span className="font-medium text-foreground">New artifact</span> to brief the agent.
				</div>
			) : (
				<ul className="flex flex-col rounded border border-border">
					{rows.map((r) => (
						<li key={r.path} className="border-b border-border last:border-b-0">
							<button
								type="button"
								onClick={() => onOpen(r.path)}
								className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm hover:bg-accent"
								title={r.path}
							>
								{r.starred ? (
									<Star className="h-3.5 w-3.5 shrink-0 fill-amber-500 text-amber-500" />
								) : (
									<FileText className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
								)}
								<span className="flex-1 truncate text-foreground">{r.name}</span>
								{r.kind && (
									<span className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
										{r.kind}
									</span>
								)}
								<span className="font-mono text-[10px] text-muted-foreground/70">
									{relativeAt(r.modified_at)}
								</span>
							</button>
						</li>
					))}
				</ul>
			)}
		</section>
	);
}

// ─── Empty / no-project state ────────────────────────────────────────────

function NoProjectState({ onNavigateSettings }: { onNavigateSettings: () => void }) {
	return (
		<div className="flex h-full items-center justify-center p-12">
			<div className="max-w-md space-y-3 text-center">
				<Sparkles className="mx-auto h-8 w-8 text-muted-foreground" />
				<h2 className="text-base font-semibold text-foreground">No active project</h2>
				<p className="text-sm text-muted-foreground">
					Pick a project to see its artifacts here. Project context drives where the agent writes
					and which skills it can use.
				</p>
				<Button size="sm" variant="outline" onClick={onNavigateSettings}>
					Open Projects settings
				</Button>
			</div>
		</div>
	);
}

// ─── Helpers ─────────────────────────────────────────────────────────────

function SectionHeading({ label, detail }: { label: string; detail?: string }) {
	return (
		<div className="flex items-baseline justify-between text-[10px] uppercase tracking-wider">
			<span className="font-medium text-muted-foreground/70">{label}</span>
			{detail && <span className="font-mono text-muted-foreground/70">{detail}</span>}
		</div>
	);
}

function relativeAt(ms: number): string {
	const delta = Date.now() - ms;
	const min = Math.round(delta / 60_000);
	if (min < 1) return 'now';
	if (min < 60) return `${min}m`;
	const hr = Math.round(min / 60);
	if (hr < 24) return `${hr}h`;
	const day = Math.round(hr / 24);
	if (day < 7) return `${day}d`;
	const wk = Math.round(day / 7);
	if (wk < 5) return `${wk}w`;
	const mo = Math.round(day / 30);
	return `${mo}mo`;
}

type LucideIcon = (typeof Icons)['Square'];

function resolveGlyph(name: string): LucideIcon {
	const map = Icons as unknown as Record<string, LucideIcon>;
	return map[name] ?? Icons.Square;
}
