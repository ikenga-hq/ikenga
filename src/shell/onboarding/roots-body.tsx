// Step 3 — Projects.
//
// One list: users pick the project folders Ikenga should know about;
// on Continue we write them to `activeProject.extra_roots`.
//
// The pre-merge version of this file maintained two side-by-side sections
// (file roots + project roots). User testing showed the distinction was
// confusing — the goal here is to ask one question.
//
// Suggestions come from a `~/.claude/projects/` scan (Rust command
// `list_claude_projects`); the decoder there now keeps any FS-verified
// prefix even when the full path can't be confirmed.

import { useQuery } from '@tanstack/react-query';
import { open as openDialog } from '@/lib/transport/dialog-shim';

import { LoreTerm } from '@/components/lore/lore-term';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { cn } from '@/components/ui/utils';
import { useShellStore } from '@/lib/shell/shell-store';
import { type ClaudeProjectEntry, listClaudeProjects } from '@/lib/tauri-cmd';
import { useEffect, useState } from 'react';

import { useOnboardingStep } from './use-onboarding-step';

export interface RootsStepPayload {
	extraRoots: string[];
}

interface RootsBodyProps {
	onContinue: () => void;
}

const CLAUDE_PROJECTS_QUERY = ['onboarding', 'claude-projects'] as const;

/**
 * Copy `paths` into the store's `extra_roots`, skipping any already present.
 * Exported so the unit test can drive the same code path the Continue
 * button does, without rendering the component.
 */
export function mirrorProjectsToFileRoots(paths: readonly string[]): void {
	const state = useShellStore.getState();
	const activeId = state.activeProjectId || 'default';
	const existing = new Set(state.activeProject?.extra_roots ?? []);
	const next = [...(state.activeProject?.extra_roots ?? [])];
	for (const p of paths) {
		if (!existing.has(p)) {
			existing.add(p);
			next.push(p);
		}
	}
	state.setProjectExtraRoots(activeId, next);
}

export function RootsBody({ onContinue }: RootsBodyProps) {
	const activeProject = useShellStore((s) => s.activeProject);
	const setProjectExtraRoots = useShellStore((s) => s.setProjectExtraRoots);
	const roots = activeProject?.extra_roots ?? [];

	function addRoot(path: string) {
		const activeId = activeProject?.id || 'default';
		if (!roots.includes(path)) {
			setProjectExtraRoots(activeId, [...roots, path]);
		}
	}

	function removeRoot(path: string) {
		const activeId = activeProject?.id || 'default';
		setProjectExtraRoots(activeId, roots.filter((r) => r !== path));
	}

	function updateRoot(oldPath: string, nextPath: string) {
		const activeId = activeProject?.id || 'default';
		setProjectExtraRoots(activeId, roots.map((r) => (r === oldPath ? nextPath : r)));
	}

	const { setPayload } = useOnboardingStep<RootsStepPayload>('roots');

	const [customProjectPath, setCustomProjectPath] = useState('');

	const { data: claudeProjects } = useQuery<ClaudeProjectEntry[]>({
		queryKey: CLAUDE_PROJECTS_QUERY,
		queryFn: listClaudeProjects,
		refetchOnWindowFocus: false,
	});

	useEffect(() => {
		setPayload({
			extraRoots: [...roots],
		});
	}, [roots, setPayload]);

	const browseProject = async () => {
		try {
			const picked = await openDialog({ directory: true, multiple: false });
			if (typeof picked === 'string' && picked.length > 0) addRoot(picked);
		} catch {
			/* swallow */
		}
	};

	// Filter out projects whose decoded path is already in the user's
	// configured roots — we only surface them as suggestions.
	const projectSuggestions = (claudeProjects ?? []).filter((p) => {
		const candidates = new Set([p.path, p.display_path]);
		return !roots.some((r) => candidates.has(r));
	});

	const handleContinue = () => {
		mirrorProjectsToFileRoots(roots);
		onContinue();
	};

	return (
		<div className="mx-auto max-w-3xl">
			<div className="mb-6">
				<h2 className="text-lg font-semibold tracking-tight text-foreground">
					Which projects should <LoreTerm term="ikenga" /> know about?
				</h2>
				<p className="mt-1 text-sm text-muted-foreground">
					We will scan each folder for <span className="font-mono text-xs">.claude/</span>{' '}
					configuration and make them reachable from the file tree. You can always add more from
					Settings later.
				</p>
			</div>

			<div className="space-y-6">
				{/* ── Project roots (Claude Code config) ── */}
				<div className="flex items-center justify-between">
					<label className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Project folders
					</label>
					<span className="text-[11px] text-muted-foreground">
						scanned by <span className="font-mono">/claude</span>
					</span>
				</div>
				<div className="grid gap-2" data-testid="project-roots-list">
					{roots.length === 0 && (
						<div
							className="rounded-md border border-dashed p-3 text-xs"
							style={{
								borderColor: 'var(--border-soft)',
								color: 'var(--fg-muted)',
							}}
						>
							No projects yet. Personal <span className="font-mono">~/.claude/</span> is always
							scanned in addition to whatever you add here.
						</div>
					)}
					{roots.map((path) => (
						<RootRow
							key={path}
							path={path}
							onRemove={() => removeRoot(path)}
							onCommit={(next) => updateRoot(path, next)}
							isDefault={false}
						/>
					))}
				</div>
				<div className="mt-3 grid grid-cols-[1fr_auto_auto] gap-2">
					<Input
						placeholder="~/Code/my-project"
						value={customProjectPath}
						onChange={(e) => setCustomProjectPath(e.target.value)}
						className="font-mono text-xs"
						data-testid="project-roots-input"
					/>
					<Button
						variant="secondary"
						onClick={() => {
							if (customProjectPath.trim()) {
								addRoot(customProjectPath.trim());
								setCustomProjectPath('');
							}
						}}
						disabled={!customProjectPath.trim()}
					>
						Add path
					</Button>
					<Button variant="secondary" onClick={browseProject} data-testid="project-roots-browse">
						Browse…
					</Button>
				</div>

				{/* ── Suggestions from ~/.claude/projects/ ────────────────── */}
				{projectSuggestions.length > 0 && (
					<div className="mt-5">
						<p
							className="mb-2 text-[11.5px] font-semibold uppercase tracking-[0.04em]"
							style={{ color: 'var(--fg-faint)' }}
						>
							Suggested from your Claude history
						</p>
						<div className="grid gap-1.5" data-testid="claude-project-suggestions">
							{projectSuggestions.slice(0, 8).map((s) => (
								<button
									key={s.slug}
									type="button"
									onClick={() => addRoot(s.path)}
									data-verified={s.path_verified}
									className="flex items-center justify-between gap-3 rounded-md border px-3 py-2 text-left transition-colors hover:border-[var(--border-strong)]"
									style={{ borderColor: 'var(--border-soft)' }}
								>
									<div className="min-w-0 flex-1">
										<div className="flex items-center gap-2">
											<span className="truncate font-mono text-[12px]" title={s.path}>
												{s.display_path}
											</span>
											{!s.path_verified && (
												<span
													className="rounded-full px-1.5 py-0.5 text-[9.5px] font-semibold uppercase tracking-wider"
													style={{
														background: 'var(--warning-soft, var(--bg-raised))',
														color: 'var(--warning, var(--fg-muted))',
													}}
													title="Best-effort guess — verify before adding"
												>
													guess
												</span>
											)}
										</div>
										<div
											className="mt-0.5 truncate font-mono text-[10.5px]"
											style={{ color: 'var(--fg-faint)' }}
											title={`Claude session dir: ${s.slug}`}
										>
											from <span className="opacity-80">~/.claude/projects/{s.slug}</span> ·{' '}
											{s.session_count} session{s.session_count === 1 ? '' : 's'}
										</div>
									</div>
									<span className="text-xs" style={{ color: 'var(--primary)' }}>
										+ Add
									</span>
								</button>
							))}
						</div>
					</div>
				)}
			</div>

			<div className="mt-8 flex items-center justify-end gap-3">
				<Button onClick={handleContinue} data-testid="roots-inline-continue">
					Continue
				</Button>
			</div>
		</div>
	);
}

interface RootRowProps {
	path: string;
	isDefault: boolean;
	onRemove: () => void;
	onCommit: (next: string) => void;
}

function RootRow({ path, isDefault, onRemove, onCommit }: RootRowProps) {
	// Local draft so the user can edit freely; commit on blur / Enter.
	// We re-sync the draft from `path` when the underlying value changes
	// (e.g. another path with the same display was removed/renamed).
	const [draft, setDraft] = useState(path);
	useEffect(() => {
		setDraft(path);
	}, [path]);

	const commit = () => {
		const trimmed = draft.trim();
		if (!trimmed || trimmed === path) {
			setDraft(path);
			return;
		}
		onCommit(trimmed);
	};

	return (
		<div
			className={cn(
				'grid grid-cols-[1fr_auto_auto] items-center gap-3 rounded-md border px-3 py-2'
			)}
			style={{
				borderColor: 'var(--border-soft)',
				background: 'var(--bg-surface)',
			}}
			data-testid="root-row"
			data-path={path}
		>
			<Input
				value={draft}
				onChange={(e) => setDraft(e.target.value)}
				onBlur={commit}
				onKeyDown={(e) => {
					if (e.key === 'Enter') {
						e.preventDefault();
						(e.target as HTMLInputElement).blur();
					} else if (e.key === 'Escape') {
						e.preventDefault();
						setDraft(path);
						(e.target as HTMLInputElement).blur();
					}
				}}
				className="h-8 border-transparent bg-transparent px-1 font-mono text-[12px] focus-visible:border-[var(--border-strong)]"
				data-testid="root-row-input"
				title={path}
			/>
			{isDefault && (
				<span
					className="rounded-full px-2 py-0.5 text-[10.5px] font-semibold uppercase tracking-wider"
					style={{ background: 'var(--bg-raised)', color: 'var(--fg-muted)' }}
				>
					default
				</span>
			)}
			<Button
				variant="ghost"
				size="sm"
				onClick={onRemove}
				className="h-7"
				data-testid="root-remove"
			>
				Remove
			</Button>
		</div>
	);
}
