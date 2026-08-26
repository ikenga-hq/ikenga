// Step 3 — Projects.
//
// One list, two derived effects. Users pick the project folders Ikenga
// should know about; on Continue we:
//   • Write them to `claudeProjectRoots` (seeds the /claude config browser).
//   • Mirror them once into `fileRoots` (the Tauri FS read/watch allowlist),
//     so users don't have to reason about the dual-list distinction during
//     onboarding. Settings still exposes both lists independently for
//     power users who want to diverge them later.
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
import { DEFAULT_CLAUDE_PROJECT_ROOTS, useShellStore } from '@/lib/shell/shell-store';
import { type ClaudeProjectEntry, listClaudeProjects } from '@/lib/tauri-cmd';
import { useEffect, useState } from 'react';

import { useOnboardingStep } from './use-onboarding-step';

export interface RootsStepPayload {
	fileRoots: string[];
	claudeProjectRoots: string[];
}

interface RootsBodyProps {
	onContinue: () => void;
}

const CLAUDE_PROJECTS_QUERY = ['onboarding', 'claude-projects'] as const;

/**
 * Copy `paths` into the store's `fileRoots`, skipping any already present.
 * Exported so the unit test can drive the same code path the Continue
 * button does, without rendering the component.
 */
export function mirrorProjectsToFileRoots(paths: readonly string[]): void {
	const state = useShellStore.getState();
	const existing = new Set(state.fileRoots);
	for (const p of paths) {
		if (!existing.has(p)) state.addFileRoot(p);
	}
}

export function RootsBody({ onContinue }: RootsBodyProps) {
	const claudeProjectRoots = useShellStore((s) => s.claudeProjectRoots);
	const addClaudeProjectRoot = useShellStore((s) => s.addClaudeProjectRoot);
	const removeClaudeProjectRoot = useShellStore((s) => s.removeClaudeProjectRoot);
	const updateClaudeProjectRoot = useShellStore((s) => s.updateClaudeProjectRoot);

	const { setPayload } = useOnboardingStep<RootsStepPayload>('roots');

	const [customProjectPath, setCustomProjectPath] = useState('');

	const { data: claudeProjects } = useQuery<ClaudeProjectEntry[]>({
		queryKey: CLAUDE_PROJECTS_QUERY,
		queryFn: listClaudeProjects,
		refetchOnWindowFocus: false,
	});

	// Keep the step payload in sync with the store so the summary screen
	// has a snapshot to render even if the user comes back later. We
	// snapshot fileRoots here too — the summary still shows both lists
	// (this step just doesn't surface file roots in its own UI any more).
	useEffect(() => {
		setPayload({
			fileRoots: [...useShellStore.getState().fileRoots],
			claudeProjectRoots: [...claudeProjectRoots],
		});
	}, [claudeProjectRoots, setPayload]);

	const browseProject = async () => {
		try {
			const picked = await openDialog({ directory: true, multiple: false });
			if (typeof picked === 'string' && picked.length > 0) addClaudeProjectRoot(picked);
		} catch {
			/* swallow */
		}
	};

	// Filter out projects whose decoded path is already in the user's
	// configured roots — we only surface them as suggestions.
	const projectSuggestions = (claudeProjects ?? []).filter((p) => {
		const candidates = new Set([p.path, p.display_path]);
		return !claudeProjectRoots.some((r) => candidates.has(r));
	});

	const handleContinue = () => {
		// Mirror once on advance — not on every keystroke. If the user
		// later removes a project from this step, the mirrored file root
		// stays put (they can prune it from Settings); that's preferable
		// to silently revoking FS access mid-edit.
		mirrorProjectsToFileRoots(claudeProjectRoots);
		onContinue();
	};

	return (
		<div className="mx-auto max-w-3xl">
			<div className="mb-6">
				<p
					className="mb-2 text-xs font-semibold uppercase tracking-[0.04em]"
					style={{ color: 'var(--primary)' }}
				>
					Your <LoreTerm term="Obi">Obi</LoreTerm>
				</p>
				<h1 className="text-3xl font-bold leading-tight tracking-tight">
					Where will your <LoreTerm term="Chi">Chi</LoreTerm> do its work?
				</h1>
				<p className="mt-2 max-w-[60ch] text-sm" style={{ color: 'var(--fg-muted)' }}>
					Your Obi is the folders your workspace calls home — Ikenga reads them and routes them in
					your sidebar. You can add more later from Settings.
				</p>
			</div>

			<section className="mb-8">
				<div className="mb-3 flex items-baseline justify-between">
					<h3 className="text-[13px] font-semibold">Projects</h3>
					<span className="text-[11.5px]" style={{ color: 'var(--fg-faint)' }}>
						scanned by <span className="font-mono">/claude</span>
					</span>
				</div>
				<div className="grid gap-2" data-testid="project-roots-list">
					{claudeProjectRoots.length === 0 && (
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
					{claudeProjectRoots.map((path) => (
						<RootRow
							key={path}
							path={path}
							onRemove={() => removeClaudeProjectRoot(path)}
							onCommit={(next) => updateClaudeProjectRoot(path, next)}
							isDefault={(DEFAULT_CLAUDE_PROJECT_ROOTS as readonly string[]).includes(path)}
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
								addClaudeProjectRoot(customProjectPath.trim());
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
									onClick={() => addClaudeProjectRoot(s.path)}
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
			</section>

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
