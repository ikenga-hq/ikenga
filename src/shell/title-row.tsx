// WP-09 — the title row (D-01, `designs/frame-workbench-v4.html` `.titlebar`;
// spec §6A.4, which supersedes §3.2).
//
// Exactly two controls: the project chip (⌘P) and the branch chip. Search is
// ⌘K; layout, the Companion toggle and notifications live in the status bar;
// window controls stay OS-native. The row itself is the window drag region.
//
// The project chip is a new component with the same behaviour as the rail's
// `ProjectIndicator` (`activity-bar.tsx`, owned by WP-03): same store
// selectors, same sort, same optimistic `setActiveProject` with the store's
// own rollback. DUPLICATION NOTE — the popover list is intentionally a copy
// until the rail indicator is removed; fold the two into one
// `ProjectSwitcherList` in the cleanup that deletes the rail copy.
//
// The branch chip reads the active project's repo through the git pkg's
// `repo.snapshot` sidecar method — the same `pkgSidecarCall` path the
// Explorer's `useGitStatus()` already uses for `changes.list`, so no new Rust
// or iyke route. When the git pkg is absent, the project has no root, or the
// root is not a repo, the chip is hidden entirely (§3.2 row 10).

import { useQuery } from '@tanstack/react-query';
import { ChevronDown, Folder, FolderKanban, GitBranch, Plus } from 'lucide-react';
import { useState } from 'react';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { cn } from '@/components/ui/utils';
import { labelFor } from '@/lib/keymap/registry';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useShellStore } from '@/lib/shell/shell-store';
import { pkgSidecarCall, type Project } from '@/lib/tauri-cmd';

const GIT_PKG_ID = 'com.ikenga.git';
/** Git pkg routes (its manifest's `ui.routes`): `/` is Changes. */
export const GIT_CHANGES_ROUTE = `/pkg/${GIT_PKG_ID}/`;
export const GIT_BRANCHES_ROUTE = `/pkg/${GIT_PKG_ID}/branches`;

// ─── git summary ───────────────────────────────────────────────────────────

export interface GitRepoSummary {
	/** Short branch name, or the short head sha when detached. */
	branch: string;
	detached: boolean;
	/** Files with any change: staged + unstaged + untracked + conflicted. */
	modified: number;
}

/** Parse the sidecar's stdout (JSON-RPC, last non-empty line) into a summary;
 *  `null` for anything that is not an `ok: true` snapshot with a head. */
export function parseRepoSnapshot(stdout: string | null | undefined): GitRepoSummary | null {
	if (!stdout) return null;
	const line = stdout
		.split('\n')
		.map((l) => l.trim())
		.filter(Boolean)
		.pop();
	if (!line) return null;
	try {
		const parsed = JSON.parse(line) as {
			result?: {
				ok?: boolean;
				snapshot?: {
					branch?: string | null;
					detached?: boolean;
					headSha?: string | null;
					staged?: number;
					unstaged?: number;
					untracked?: number;
					conflicted?: number;
				};
			};
		};
		const snap = parsed.result?.ok === true ? parsed.result.snapshot : undefined;
		if (!snap) return null;
		const branch = snap.branch ?? (snap.headSha ? snap.headSha.slice(0, 7) : null);
		if (!branch) return null;
		const n = (v: number | undefined) => (typeof v === 'number' && v > 0 ? v : 0);
		return {
			branch,
			detached: snap.detached === true || !snap.branch,
			modified: n(snap.staged) + n(snap.unstaged) + n(snap.untracked) + n(snap.conflicted),
		};
	} catch {
		return null;
	}
}

/** Branch + modified count for the active project's root. Shared by the
 *  title-row branch chip and the status bar (one query key, one poll). */
export function useGitRepoSummary() {
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const rootPath = useShellStore(
		(s) => s.projects.find((p) => p.id === s.activeProjectId)?.root_path ?? null
	);
	return useQuery<GitRepoSummary | null>({
		queryKey: ['git-repo-summary', activeProjectId, rootPath],
		enabled: !!rootPath,
		queryFn: async () => {
			if (!rootPath) return null;
			const stdin = JSON.stringify({
				jsonrpc: '2.0',
				id: 1,
				method: 'repo.snapshot',
				params: { repo: rootPath },
			});
			try {
				// Same two-name fallback as `useGitStatus()`.
				let res = await pkgSidecarCall(GIT_PKG_ID, 'pa-com-ikenga-git-repo', [], {
					stdin,
					timeoutSecs: 5,
				});
				if (!res?.ok) {
					res = await pkgSidecarCall(GIT_PKG_ID, 'default', [], { stdin, timeoutSecs: 5 });
				}
				if (!res?.ok) return null;
				return parseRepoSnapshot(res.stdout);
			} catch {
				return null;
			}
		},
		refetchInterval: 10_000,
		staleTime: 5_000,
	});
}

// ─── shared chip styling (tokens only) ─────────────────────────────────────

const CHIP =
	'flex h-7 items-center gap-2 rounded-[var(--radius-sm)] border border-transparent px-2 text-foreground outline-none transition-colors duration-[var(--motion-fast)] ease-[var(--ease-calm)] motion-reduce:transition-none hover:bg-[var(--bg-raised)] active:bg-[var(--border-soft)] focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset aria-expanded:border-border aria-expanded:bg-[var(--bg-raised)]';

// ─── project chip ──────────────────────────────────────────────────────────

function sortProjects(projects: Project[], activeProjectId: string | null): Project[] {
	// Same order as the rail indicator and `/settings/projects`: active first,
	// archived last, then position, then creation time.
	return projects.slice().sort((a, b) => {
		if (a.id === activeProjectId) return -1;
		if (b.id === activeProjectId) return 1;
		const aArc = a.archived_at != null ? 1 : 0;
		const bArc = b.archived_at != null ? 1 : 0;
		if (aArc !== bArc) return aArc - bArc;
		if (a.position !== b.position) return a.position - b.position;
		return a.created_at - b.created_at;
	});
}

export function ProjectChip() {
	const [open, setOpen] = useState(false);
	const projects = useShellStore((s) => s.projects);
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const setActiveProject = useShellStore((s) => s.setActiveProject);
	const active = projects.find((p) => p.id === activeProjectId);
	const sorted = sortProjects(projects, activeProjectId);
	const switchHint = labelFor('palette.projects');

	async function pick(id: string) {
		setOpen(false);
		try {
			await setActiveProject(id);
		} catch {
			// The store's optimistic flip already rolled back on error.
		}
	}

	function openProjectsSettings() {
		setOpen(false);
		usePaneStore.getState().navigateFocused('/settings/projects');
	}

	const name = active?.display_name ?? 'No project';
	return (
		<Popover open={open} onOpenChange={setOpen}>
			<PopoverTrigger asChild>
				<button
					type="button"
					data-testid="title-project-chip"
					aria-label={`Project: ${name} — switch project (${switchHint})`}
					title={active?.root_path ?? undefined}
					className={CHIP}
				>
					<Folder aria-hidden className="h-3.5 w-3.5 text-[var(--tint-files-fg)]" />
					<span className="max-w-[18rem] truncate text-sm font-semibold tracking-tight">
						{name}
					</span>
					<ChevronDown aria-hidden className="h-3.5 w-3.5 text-muted-foreground" />
					<kbd
						aria-hidden
						className="font-mono text-[length:var(--text-micro)] text-muted-foreground"
					>
						{switchHint}
					</kbd>
				</button>
			</PopoverTrigger>
			<PopoverContent side="bottom" align="start" className="w-64 p-2">
				<div className="px-2 pb-2 pt-1 text-xs font-medium text-muted-foreground">
					Switch project
				</div>
				<ul className="flex max-h-72 flex-col overflow-y-auto">
					{sorted.map((p) => (
						<li key={p.id}>
							<button
								type="button"
								onClick={() => void pick(p.id)}
								aria-current={p.id === activeProjectId ? 'true' : undefined}
								className={cn(
									'flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2 py-1.5 text-left text-sm outline-none transition-colors motion-reduce:transition-none',
									'hover:bg-accent hover:text-accent-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset',
									p.id === activeProjectId && 'bg-accent/60 font-medium',
									p.archived_at != null && 'opacity-60'
								)}
							>
								<span
									aria-hidden
									className="inline-block h-3 w-3 shrink-0 rounded-full border border-border"
									style={{ background: p.color ?? 'var(--fg-faint)' }}
								/>
								{p.icon && <span className="text-sm leading-none">{p.icon}</span>}
								<span className="flex-1 truncate">{p.display_name}</span>
								{p.id === activeProjectId && (
									<span className="text-xs text-muted-foreground">Active</span>
								)}
								{p.archived_at != null && (
									<span className="text-xs text-muted-foreground">Archived</span>
								)}
							</button>
						</li>
					))}
					{sorted.length === 0 && (
						<li className="px-2 py-3 text-center text-xs text-muted-foreground">
							Loading projects…
						</li>
					)}
				</ul>
				<div className="mt-2 border-t border-border pt-2">
					<button
						type="button"
						onClick={openProjectsSettings}
						className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2 py-1.5 text-left text-sm text-muted-foreground outline-none transition-colors motion-reduce:transition-none hover:bg-accent hover:text-accent-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
					>
						<Plus aria-hidden className="h-3.5 w-3.5" />
						New project…
					</button>
					<button
						type="button"
						onClick={openProjectsSettings}
						className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2 py-1.5 text-left text-sm text-muted-foreground outline-none transition-colors motion-reduce:transition-none hover:bg-accent hover:text-accent-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
					>
						<FolderKanban aria-hidden className="h-3.5 w-3.5" />
						Manage projects…
					</button>
				</div>
			</PopoverContent>
		</Popover>
	);
}

// ─── branch chip ───────────────────────────────────────────────────────────

export function BranchChip() {
	const { data } = useGitRepoSummary();
	if (!data) return null;
	return (
		<button
			type="button"
			data-testid="title-branch-chip"
			onClick={() => usePaneStore.getState().navigateFocused(GIT_BRANCHES_ROUTE)}
			aria-label={`Branch: ${data.branch}${data.detached ? ' (detached)' : ''} — open branches`}
			className={cn(CHIP, 'text-muted-foreground')}
		>
			<GitBranch aria-hidden className="h-3.5 w-3.5" />
			<span className="max-w-[14rem] truncate font-mono text-[length:var(--text-caption)] font-medium">
				{data.branch}
			</span>
			<ChevronDown aria-hidden className="h-3.5 w-3.5" />
		</button>
	);
}

// ─── the row ───────────────────────────────────────────────────────────────

export function TitleRow() {
	return (
		<div
			role="toolbar"
			aria-label="Title row"
			data-testid="title-row"
			data-tauri-drag-region
			className="flex h-[38px] flex-none items-center gap-3 border-b border-border bg-[var(--bg-surface)] px-3"
		>
			<ProjectChip />
			<BranchChip />
		</div>
	);
}
