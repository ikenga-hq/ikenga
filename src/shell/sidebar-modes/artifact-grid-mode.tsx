// Artifact-grid sidebar.
//
// Activated by the activity-bar Artifact-grid icon (⌘5). Body shows the
// recently-opened folders the user has been working in — same data as the
// activity-bar quick-launcher popover — plus a tools row for creating a new
// artifact or browsing to a fresh folder. A slim catalog stripe at the top
// (total, drafts, starred) is fed by the project-wide artifact walker.

import { useEffect, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
	ChevronDown,
	ChevronLeft,
	FileText,
	Folder,
	FolderCog,
	FolderOpen,
	Home,
	Plus,
	Trash2,
} from 'lucide-react';
import { open as openDialog } from '@/lib/transport/dialog-shim';

import { ListRow, RowAction } from '@/components/ui/list-row';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { cn } from '@/components/ui/utils';
import { findLeaf } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import { projectArtifactsQueryOptions } from '@/lib/queries/project-artifacts';
import {
	type RecentArtifact,
	loadRecentArtifacts,
	removeRecentArtifact,
	subscribeRecentArtifacts,
} from '@/lib/shell/artifact-grid-recent-artifacts';
import {
	type RecentGridFolder,
	loadRecents,
	openArtifactGrid,
	removeRecent,
	subscribeRecents,
} from '@/lib/shell/artifact-grid-recents';
import { useShellStore } from '@/lib/shell/shell-store';
import { type FileEntry, fsList, type Project } from '@/lib/tauri-cmd';
import { useShallow } from 'zustand/react/shallow';

export function ArtifactGridMode() {
	const navigateFocused = usePaneStore((s) => s.navigateFocused);
	const projects = useShellStore((s) => s.projects);
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const activeProject = projects.find((p) => p.id === activeProjectId) ?? null;
	const setActiveProject = useShellStore((s) => s.setActiveProject);

	// Detect when the focused pane is showing an artifact-studio view so we
	// can swap the sidebar body from recents → folder navigator. `useShallow`
	// keeps this from re-rendering on every unrelated pane mutation.
	const focusedFolder = usePaneStore(
		useShallow((s) => {
			const leaf = findLeaf(s.root, s.focusedId);
			if (!leaf) return null;
			const tab = leaf.tabs[leaf.activeTabIdx];
			if (!tab || tab.kind !== 'artifact-studio') return null;
			if (tab.density === 'grid') return tab.path;
			if (tab.density === 'loupe') return parentDir(tab.path);
			if (tab.density === 'compare') return parentDir(tab.path);
			return null;
		})
	);

	const catalogQuery = useQuery(projectArtifactsQueryOptions(activeProject?.root_path ?? null));
	const counts = catalogQuery.data?.counts;

	const [recents, setRecents] = useState<RecentGridFolder[]>([]);
	const [recentArtifacts, setRecentArtifacts] = useState<RecentArtifact[]>([]);
	const [hydrated, setHydrated] = useState(false);

	// Per-project recents. Reloads on project switch via the projectId
	// dependency. Subscribe channel is scoped per project so a write
	// elsewhere live-refreshes the matching list.
	useEffect(() => {
		if (!activeProjectId) return;
		let cancelled = false;
		setHydrated(false);
		Promise.all([loadRecents(activeProjectId), loadRecentArtifacts(activeProjectId)]).then(
			([folders, artifacts]) => {
				if (cancelled) return;
				setRecents(folders);
				setRecentArtifacts(artifacts);
				setHydrated(true);
			}
		);
		const unsubFolders = subscribeRecents(activeProjectId, setRecents);
		const unsubArtifacts = subscribeRecentArtifacts(activeProjectId, setRecentArtifacts);
		return () => {
			cancelled = true;
			unsubFolders();
			unsubArtifacts();
		};
	}, [activeProjectId]);

	async function openFolder(path: string) {
		if (!activeProjectId) return;
		await openArtifactGrid(activeProjectId, path);
	}

	async function dropFolder(path: string) {
		if (!activeProjectId) return;
		await removeRecent(activeProjectId, path);
	}

	function openArtifactLoupe(path: string) {
		const ps = usePaneStore.getState();
		// If the focused pane is already a loupe, swap in place — clicking a
		// sibling/version artifact in the sidebar should feel like changing
		// frames, not piling up tabs. Otherwise mount as a new tab.
		const leaf = findLeaf(ps.root, ps.focusedId);
		const active = leaf?.tabs[leaf.activeTabIdx];
		if (active?.kind === 'artifact-studio' && active.density === 'loupe') {
			ps.replaceActiveViewAndPushHistory(ps.focusedId, {
				kind: 'artifact-studio',
				path,
				density: 'loupe',
			});
		} else {
			ps.addTab(ps.focusedId, { kind: 'artifact-studio', path, density: 'loupe' });
		}
	}

	async function dropArtifact(path: string) {
		if (!activeProjectId) return;
		await removeRecentArtifact(activeProjectId, path);
	}

	async function browse() {
		if (!activeProjectId) return;
		try {
			const picked = await openDialog({ directory: true, multiple: false });
			if (typeof picked === 'string' && picked.length > 0) {
				await openArtifactGrid(activeProjectId, picked);
			}
		} catch (e) {
			console.error('[artifact-grid] folder-picker failed', e);
		}
	}

	function openWizard() {
		navigateFocused('/projects/new-artifact');
	}

	function openHome() {
		navigateFocused('/artifacts/home');
	}

	function openProjectSettings() {
		navigateFocused('/settings/projects');
	}

	return (
		<div className="flex h-full flex-col">
			{/* Project chip — popover switcher for the active project. */}
			<ProjectChip
				projects={projects}
				activeProjectId={activeProjectId}
				activeProject={activeProject}
				onPick={(id) => {
					void setActiveProject(id).catch(() => {
						// Store handles its own rollback on failure.
					});
				}}
				onOpenSettings={openProjectSettings}
			/>

			{/* Catalog stripe — total / starred / recent across the project. */}
			{activeProject?.root_path && counts && (
				<div className="flex items-center gap-4 border-b border-border px-4 py-1.5 text-[10px] text-muted-foreground">
					<CountChip label="all" value={counts.all} />
					{counts.starred > 0 && <CountChip label="starred" value={counts.starred} />}
					{counts.recent > 0 && <CountChip label="recent" value={counts.recent} />}
				</div>
			)}

			{focusedFolder ? (
				<FolderNavigator
					folder={focusedFolder}
					onBackToRecents={() => navigateFocused('/artifacts/home')}
					onOpenSubfolder={(p) => void openFolder(p)}
					onOpenArtifact={openArtifactLoupe}
				/>
			) : (
				/* Recents — full-height body. Recent artifacts first (loupe
				   opens), then recent folders (grid opens). Both scoped to the
				   active project. */
				<div className="flex-1 min-h-0 overflow-y-auto">
					{recentArtifacts.length > 0 && (
						<>
							<SectionLabel>Recent artifacts</SectionLabel>
							<ul className="flex flex-col">
								{recentArtifacts.map((r) => (
									<RecentArtifactRow
										key={r.path}
										artifact={r}
										onOpen={() => openArtifactLoupe(r.path)}
										onDrop={() => void dropArtifact(r.path)}
									/>
								))}
							</ul>
						</>
					)}

					<SectionLabel>Recent folders</SectionLabel>
					{!hydrated ? (
						<div className="px-4 py-2 text-xs italic text-muted-foreground">Loading…</div>
					) : recents.length === 0 ? (
						<div className="px-4 py-3 text-xs italic text-muted-foreground">
							No recent folders. Use{' '}
							<button
								type="button"
								onClick={() => void browse()}
								className="underline hover:text-foreground"
							>
								Browse folder…
							</button>{' '}
							to pick one.
						</div>
					) : (
						<ul className="flex flex-col">
							{recents.map((r) => (
								<RecentRow
									key={r.path}
									recent={r}
									onOpen={() => void openFolder(r.path)}
									onDrop={() => void dropFolder(r.path)}
								/>
							))}
						</ul>
					)}
				</div>
			)}

			{/* Tools row — pinned to the bottom of the sidebar. */}
			<div className="border-t border-border">
				<button
					type="button"
					onClick={openHome}
					className={cn(
						'flex w-full items-center gap-3 px-4 py-2 text-left text-sm',
						'text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground'
					)}
				>
					<Home className="h-4 w-4 shrink-0" />
					<span className="flex-1">Home</span>
				</button>
				<button
					type="button"
					onClick={openWizard}
					className={cn(
						'flex w-full items-center gap-3 px-4 py-2 text-left text-sm',
						'text-foreground transition-colors hover:bg-accent hover:text-accent-foreground'
					)}
				>
					<Plus className="h-4 w-4 shrink-0" />
					<span className="flex-1">New artifact</span>
					<span className="font-mono text-[10px] text-muted-foreground">⌘⇧N</span>
				</button>
				<button
					type="button"
					onClick={() => void browse()}
					className={cn(
						'flex w-full items-center gap-3 px-4 py-2 text-left text-sm',
						'text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground'
					)}
				>
					<FolderOpen className="h-4 w-4 shrink-0" />
					<span className="flex-1">Browse folder…</span>
				</button>
			</div>
		</div>
	);
}

function RecentRow({
	recent,
	onOpen,
	onDrop,
}: {
	recent: RecentGridFolder;
	onOpen: () => void;
	onDrop: () => void;
}) {
	const name = recent.path.replace(/\/+$/, '').replace(/^.+\//, '') || recent.path;
	return (
		<li>
			<ListRow
				size="lg"
				onActivate={onOpen}
				title={recent.path}
				icon={<Folder className="h-4 w-4 shrink-0 text-muted-foreground" />}
				name={name}
				timestamp={relativeOpenedAt(recent.openedAtMs)}
				actions={
					<RowAction
						icon={<Trash2 className="h-3 w-3" />}
						label="Remove from recents"
						onClick={onDrop}
					/>
				}
			/>
		</li>
	);
}

function RecentArtifactRow({
	artifact,
	onOpen,
	onDrop,
}: {
	artifact: RecentArtifact;
	onOpen: () => void;
	onDrop: () => void;
}) {
	const name = artifact.path
		.replace(/\/+$/, '')
		.replace(/^.+\//, '')
		.replace(/\.html?$/i, '');
	return (
		<li>
			<ListRow
				size="lg"
				onActivate={onOpen}
				title={artifact.path}
				icon={<FileText className="h-4 w-4 shrink-0 text-muted-foreground" />}
				name={name}
				timestamp={relativeOpenedAt(artifact.openedAtMs)}
				actions={
					<RowAction
						icon={<Trash2 className="h-3 w-3" />}
						label="Remove from recents"
						onClick={onDrop}
					/>
				}
			/>
		</li>
	);
}

function SectionLabel({ children }: { children: React.ReactNode }) {
	return (
		<div className="px-4 pb-1 pt-3 text-[10px] font-medium uppercase tracking-wider text-muted-foreground/60">
			{children}
		</div>
	);
}

function ProjectChip({
	projects,
	activeProjectId,
	activeProject,
	onPick,
	onOpenSettings,
}: {
	projects: Project[];
	activeProjectId: string;
	activeProject: Project | null;
	onPick: (id: string) => void;
	onOpenSettings: () => void;
}) {
	const [open, setOpen] = useState(false);

	// Active first, then non-archived (by position then created_at), then
	// archived last. Same ordering as the activity-bar project switcher
	// and Settings → Projects.
	const sorted = projects.slice().sort((a, b) => {
		if (a.id === activeProjectId) return -1;
		if (b.id === activeProjectId) return 1;
		const aArc = a.archived_at != null ? 1 : 0;
		const bArc = b.archived_at != null ? 1 : 0;
		if (aArc !== bArc) return aArc - bArc;
		if (a.position !== b.position) return a.position - b.position;
		return a.created_at - b.created_at;
	});

	function pick(id: string) {
		setOpen(false);
		if (id !== activeProjectId) onPick(id);
	}

	function settings() {
		setOpen(false);
		onOpenSettings();
	}

	return (
		<Popover open={open} onOpenChange={setOpen}>
			<PopoverTrigger asChild>
				<button
					type="button"
					className="flex items-center gap-2 border-b border-border px-4 py-2 text-left text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground"
					title={activeProject?.root_path ?? 'No active project'}
				>
					<span
						aria-hidden
						className="inline-block h-2.5 w-2.5 shrink-0 rounded-full border border-border"
						style={{ background: activeProject?.color ?? '#7c7c7c' }}
					/>
					<span className="flex-1 truncate font-medium text-foreground">
						{activeProject?.display_name ?? 'No project'}
					</span>
					{activeProject?.root_path && (
						<span className="truncate font-mono text-[10px] opacity-70">
							{shortRoot(activeProject.root_path)}
						</span>
					)}
					<ChevronDown className="h-3 w-3 shrink-0 opacity-70" />
				</button>
			</PopoverTrigger>
			<PopoverContent side="right" align="start" className="w-64 p-1">
				<div className="px-2 pb-1 pt-1 text-[10px] font-medium uppercase tracking-wider text-muted-foreground/70">
					Switch project
				</div>
				<ul className="flex max-h-72 flex-col overflow-y-auto">
					{sorted.map((p) => (
						<li key={p.id}>
							<button
								type="button"
								onClick={() => pick(p.id)}
								className={cn(
									'flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm transition-colors',
									'hover:bg-accent hover:text-accent-foreground',
									p.id === activeProjectId && 'bg-accent/60 font-medium',
									p.archived_at != null && 'opacity-60'
								)}
							>
								<span
									aria-hidden
									className="inline-block h-2.5 w-2.5 shrink-0 rounded-full border border-border"
									style={{ background: p.color ?? '#7c7c7c' }}
								/>
								<span className="flex-1 truncate text-foreground">{p.display_name}</span>
								{p.archived_at != null && (
									<span className="font-mono text-[9px] uppercase text-muted-foreground">
										archived
									</span>
								)}
							</button>
						</li>
					))}
				</ul>
				<div className="my-1 h-px bg-border" aria-hidden />
				<button
					type="button"
					onClick={settings}
					className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground"
				>
					<FolderCog className="h-3.5 w-3.5" />
					Project settings…
				</button>
			</PopoverContent>
		</Popover>
	);
}

function CountChip({ label, value }: { label: string; value: number }) {
	return (
		<span className="inline-flex items-baseline gap-1">
			<span className="font-mono text-[11px] text-foreground">{value}</span>
			<span className="uppercase tracking-wider text-muted-foreground/60">{label}</span>
		</span>
	);
}

// ─── Folder navigator ────────────────────────────────────────────────────

function FolderNavigator({
	folder,
	onBackToRecents,
	onOpenSubfolder,
	onOpenArtifact,
}: {
	folder: string;
	onBackToRecents: () => void;
	onOpenSubfolder: (path: string) => void;
	onOpenArtifact: (path: string) => void;
}) {
	const folderName = folder.replace(/\/+$/, '').replace(/^.+\//, '') || folder;

	const list = useQuery({
		queryKey: ['artifact-grid-nav', folder] as const,
		queryFn: () => fsList(folder),
		staleTime: 10_000,
	});

	const { dirs, files } = splitEntries(list.data ?? []);

	return (
		<div className="flex flex-1 min-h-0 flex-col">
			<button
				type="button"
				onClick={onBackToRecents}
				className="flex shrink-0 items-center gap-2 border-b border-border px-4 py-1.5 text-left text-[11px] text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground"
				title="Back to recents"
			>
				<ChevronLeft className="h-3 w-3 shrink-0" />
				<span className="truncate">Recents</span>
			</button>
			<div className="flex shrink-0 items-center gap-2 border-b border-border px-4 py-2 text-xs">
				<FolderOpen className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
				<span className="flex-1 truncate font-medium text-foreground" title={folder}>
					{folderName}
				</span>
				<span className="font-mono text-[10px] text-muted-foreground/70">
					{dirs.length}d · {files.length}f
				</span>
			</div>
			<div className="flex-1 min-h-0 overflow-y-auto">
				{list.isLoading && !list.data ? (
					<div className="px-4 py-2 text-xs italic text-muted-foreground">Loading…</div>
				) : list.error ? (
					<div className="px-4 py-2 text-xs italic text-destructive">Could not read folder.</div>
				) : dirs.length === 0 && files.length === 0 ? (
					<div className="px-4 py-3 text-xs italic text-muted-foreground">Empty folder.</div>
				) : (
					<ul className="flex flex-col">
						{dirs.map((d) => (
							<NavRow
								key={d.path}
								icon={<Folder className="h-4 w-4 shrink-0 text-muted-foreground" />}
								name={d.name}
								title={d.path}
								onClick={() => onOpenSubfolder(d.path)}
							/>
						))}
						{files.map((f) => (
							<NavRow
								key={f.path}
								icon={<FileText className="h-4 w-4 shrink-0 text-muted-foreground" />}
								name={f.name.replace(/\.html?$/i, '')}
								title={f.path}
								onClick={() => onOpenArtifact(f.path)}
							/>
						))}
					</ul>
				)}
			</div>
		</div>
	);
}

function NavRow({
	icon,
	name,
	title,
	onClick,
}: {
	icon: React.ReactNode;
	name: string;
	title: string;
	onClick: () => void;
}) {
	return (
		<li>
			<button
				type="button"
				onClick={onClick}
				className="flex w-full min-w-0 items-center gap-2 px-4 py-1.5 text-left text-sm text-foreground transition-colors hover:bg-accent hover:text-accent-foreground"
				title={title}
			>
				{icon}
				<span className="flex-1 min-w-0 truncate">{name}</span>
			</button>
		</li>
	);
}

/** Sort: subfolders alpha first, then `.html` / `.htm` files alpha. Other
 *  file types are hidden — this sidebar is the artifact navigator, not a
 *  general file explorer. */
function splitEntries(entries: FileEntry[]): { dirs: FileEntry[]; files: FileEntry[] } {
	const dirs: FileEntry[] = [];
	const files: FileEntry[] = [];
	for (const e of entries) {
		if (e.name.startsWith('.')) continue;
		if (e.isDir) {
			dirs.push(e);
		} else if (/\.html?$/i.test(e.name)) {
			files.push(e);
		}
	}
	dirs.sort((a, b) => a.name.localeCompare(b.name));
	files.sort((a, b) => a.name.localeCompare(b.name));
	return { dirs, files };
}

// ─── Helpers ─────────────────────────────────────────────────────────────

function parentDir(p: string): string {
	const trimmed = p.replace(/\/+$/, '');
	const idx = trimmed.lastIndexOf('/');
	if (idx <= 0) return '/';
	return trimmed.slice(0, idx);
}

function relativeOpenedAt(ms: number): string {
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

function shortRoot(root: string): string {
	const home = root.match(/^\/home\/[^/]+/)?.[0];
	if (home && root.startsWith(home)) return `~${root.slice(home.length)}`;
	return root;
}
