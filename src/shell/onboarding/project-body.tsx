// Step 3 (D-04 `project`) — "Open a project." Rebuilt to
// `designs/onboarding.html` `VIEW.project` (WP-38 conformance review F0/F1):
//
//   col A  "Detected repositories" — a single-select radiogroup of the
//          repos found under `~/.claude/projects/` (Rust
//          `list_claude_projects`), plus "Start empty"; footer "Open a
//          folder…". Below it the "Extra roots" disclosure, then the Writes
//          line.
//   col B  "What was detected inside" the selection (project-scope
//          skills / agents / hooks / MCP tools / commands, via
//          `claude_config_load`) and the "Scope" note.
//   acts   the Personal / Project scope switch (`SettingsScopeSwitch`, the
//          WP-35 settings primitive), shared with the `equipment` step.
//
// Write map (everything the shipped `roots` step wrote, preserved):
//   - `activeProject.extra_roots` via `setProjectExtraRoots` — now through
//     the "Extra roots" disclosure, landing in the settings.json the scope
//     switch names (unchanged default: project when writable, else personal).
//   - the step payload's `extraRoots` (read by `done`) — written by the
//     commit only, never live, so leaving via Back / the rail records nothing.
//   (The old `mirrorProjectsToFileRoots` helper had no callers left once
//   the commit below took over its write.)
// New, per D-04 ("one becomes activeProject"): a detected / picked repo is
// matched to an existing project by root (or created with `project_create`)
// and made active. "Start empty" writes nothing. Both happen on Continue —
// footer or inline — via the wizard's `setBeforeNext` hook.

import { useQuery } from '@tanstack/react-query';
import { Box, ChevronRight, FileText, Folder, FolderOpen } from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';

import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { cn } from '@/components/ui/utils';
import { labelFor } from '@/lib/keymap/registry';
import { openSettingsFile } from '@/lib/settings/client';
import { dedupeRoots, useShellStore } from '@/lib/shell/shell-store';
import {
	type ClaudeConfig,
	type ClaudeProjectEntry,
	type Project,
	claudeConfigLoad,
	listClaudeProjects,
	projectCreate,
} from '@/lib/tauri-cmd';
import { open as openDialog } from '@/lib/transport/dialog-shim';
import { WritesNote } from '@/shell/onboarding/footer';
import { effectiveOnboardingScope, useOnboardingScope } from '@/shell/onboarding/scope';
import type { BeforeNext } from '@/shell/onboarding/wizard-stepper';
import { settingsPathLabel } from '@/shell/settings/header';
import type { SettingsScopeId } from '@/shell/settings/nav';
import { SettingsScopeSwitch } from '@/shell/settings/scope-switch';

import { useOnboardingStep } from './use-onboarding-step';

export interface ProjectStepPayload {
	extraRoots: string[];
	/** `detected` — a repo was opened; `empty` — "Start empty". */
	mode?: 'detected' | 'empty';
	projectId?: string | null;
	projectName?: string | null;
	projectRoot?: string | null;
	scope?: SettingsScopeId;
}

interface ProjectBodyProps {
	onContinue: () => void;
	registerBeforeNext: (fn: BeforeNext | null) => void;
}

const CLAUDE_PROJECTS_QUERY = ['onboarding', 'claude-projects'] as const;
const EMPTY = '__empty';
const MAX_DETECTED = 8;

interface Candidate {
	path: string;
	name: string;
	sessions: number | null;
	verified: boolean;
}

function normalizePath(p: string): string {
	return p.replace(/[/\\]+$/, '');
}

function baseName(p: string): string {
	return normalizePath(p).split(/[/\\]/).pop() || p;
}

function projectForRoot(projects: readonly Project[], path: string): Project | undefined {
	const target = normalizePath(path);
	return projects.find(
		(p) => p.archived_at == null && p.root_path != null && normalizePath(p.root_path) === target
	);
}

/** `project_create` ids match `/^[a-z0-9][a-z0-9_-]{0,63}$/` (see
 *  `routes/settings/projects.tsx`); suffix until unused, archived included. */
function uniqueProjectId(name: string, projects: readonly Project[]): string {
	const base =
		name
			.toLowerCase()
			.replace(/[^a-z0-9_-]+/g, '-')
			.replace(/^[^a-z0-9]+/, '')
			.replace(/-+$/, '')
			.slice(0, 56) || 'project';
	const taken = new Set(projects.map((p) => p.id));
	let id = base;
	for (let n = 2; taken.has(id); n++) id = `${base}-${n}`;
	return id;
}

export function ProjectBody({ onContinue, registerBeforeNext }: ProjectBodyProps) {
	const activeProject = useShellStore((s) => s.activeProject);
	const projects = useShellStore((s) => s.projects);
	const projectExtraRoots = useShellStore((s) => s.projectExtraRoots);
	const { record, setPayload } = useOnboardingStep<ProjectStepPayload>('project');
	// The answer from a previous visit (the payload is written only by the
	// commit below), frozen at mount so this visit's commit doesn't re-seed
	// the pick mid-render.
	const [prior] = useState(() => record.payload);

	const { data: claudeProjects, isLoading: scanning } = useQuery<ClaudeProjectEntry[]>({
		queryKey: CLAUDE_PROJECTS_QUERY,
		queryFn: listClaudeProjects,
		refetchOnWindowFocus: false,
	});

	// Folders picked with "Open a folder…" join the list as selectable rows.
	const [picked, setPicked] = useState<string[]>([]);

	const candidates = useMemo<Candidate[]>(() => {
		const out: Candidate[] = [];
		const seen = new Set<string>();
		const push = (c: Candidate) => {
			const key = normalizePath(c.path);
			if (seen.has(key)) return;
			seen.add(key);
			out.push(c);
		};
		for (const p of picked) push({ path: p, name: baseName(p), sessions: null, verified: true });
		if (prior?.mode === 'detected' && prior.projectRoot) {
			const root = prior.projectRoot;
			push({ path: root, name: prior.projectName ?? baseName(root), sessions: null, verified: true });
		}
		if (activeProject.root_path) {
			const own = projects.find((p) => p.id === activeProject.id);
			push({
				path: activeProject.root_path,
				name: own?.display_name ?? baseName(activeProject.root_path),
				sessions: null,
				verified: true,
			});
		}
		const detected = [...(claudeProjects ?? [])].sort(
			(a, b) =>
				Number(b.path_verified) - Number(a.path_verified) || b.last_modified_ms - a.last_modified_ms
		);
		for (const d of detected) {
			if (out.length >= MAX_DETECTED + picked.length + 1) break;
			push({
				path: d.path,
				name: baseName(d.display_path || d.path),
				sessions: d.session_count,
				verified: d.path_verified,
			});
		}
		return out;
	}, [activeProject.id, activeProject.root_path, claudeProjects, picked, prior, projects]);

	// Explicit pick → else what this step last committed → else the active
	// project's root → else the first verified detection → else Start empty.
	const [picks, setPicks] = useState<string | null>(null);
	const selection: string =
		picks ??
		(prior?.mode === 'empty'
			? EMPTY
			: (prior?.projectRoot ??
				activeProject.root_path ??
				candidates.find((c) => c.verified)?.path ??
				EMPTY));
	const isEmpty = selection === EMPTY;
	const selected = isEmpty ? undefined : candidates.find((c) => c.path === selection);
	const selectedPath = isEmpty ? null : selection;
	const existingProject = selectedPath ? projectForRoot(projects, selectedPath) : undefined;

	// ── Scope (shared with `equipment`) ─────────────────────────────────
	const explicitScope = useOnboardingScope((s) => s.explicit);
	const setScope = useOnboardingScope((s) => s.setScope);
	const scopeRoot = selectedPath ?? activeProject.root_path;
	const scope = effectiveOnboardingScope(explicitScope, scopeRoot);
	const scopeFile = settingsPathLabel(scope, scopeRoot);
	const scopeProjectId = selectedPath ? (existingProject?.id ?? null) : activeProject.id;

	// ── Extra roots (the target project's own roots; carried v15 roots
	// apply to every project and aren't edited here) ─────────────────────
	const targetId = selectedPath ? (existingProject?.id ?? null) : activeProject.id;
	const targetRoots = targetId ? (projectExtraRoots[targetId] ?? []) : [];
	const [draftRoots, setDraftRoots] = useState<string[] | null>(null);
	const roots = draftRoots ?? targetRoots;
	const [rootsOpen, setRootsOpen] = useState(false);
	const [rootInput, setRootInput] = useState('');

	const addRoot = (path: string) => {
		const p = path.trim();
		if (!p || roots.includes(p)) return;
		setDraftRoots([...roots, p]);
	};
	const removeRoot = (path: string) => setDraftRoots(roots.filter((r) => r !== path));

	// ── What was detected inside the selection ──────────────────────────
	const { data: inside, isLoading: insideLoading } = useQuery<ClaudeConfig>({
		enabled: !!selectedPath,
		queryKey: ['onboarding', 'project-inside', selectedPath],
		queryFn: () => claudeConfigLoad([selectedPath as string]),
		refetchOnWindowFocus: false,
	});
	const insideCounts = useMemo(() => {
		if (!inside) return null;
		const own = <T extends { scope: string }>(xs: T[]) => xs.filter((x) => x.scope === 'project').length;
		return {
			skills: own(inside.skills),
			agents: own(inside.agents),
			hooks: own(inside.hooks),
			mcp: own(inside.mcps),
			commands: own(inside.commands),
		};
	}, [inside]);

	// Written to the step record ONLY by the commit below. The selection
	// defaults to the first verified repo before the user picks anything, so
	// a live write would record a project the user never chose whenever they
	// left via Back or the rail.
	const payload: ProjectStepPayload = {
		extraRoots: [...roots],
		mode: isEmpty ? 'empty' : 'detected',
		projectId: isEmpty ? activeProject.id : (existingProject?.id ?? null),
		projectName: isEmpty ? null : (existingProject?.display_name ?? selected?.name ?? null),
		projectRoot: selectedPath,
		scope,
	};

	// ── Commit on Continue (footer or inline) ───────────────────────────
	const commit: BeforeNext = async () => {
		let projectId = useShellStore.getState().activeProjectId || 'default';
		let committed = payload;
		if (selectedPath) {
			let project = projectForRoot(useShellStore.getState().projects, selectedPath);
			if (!project) {
				const name = selected?.name ?? baseName(selectedPath);
				project = await projectCreate({
					id: uniqueProjectId(name, useShellStore.getState().projects),
					display_name: name,
					root_path: selectedPath,
				});
				await useShellStore.getState().refreshProjects();
			}
			await useShellStore.getState().setActiveProject(project.id);
			projectId = project.id;
			committed = { ...payload, projectId, projectName: project.display_name };
		}
		if (draftRoots !== null || explicitScope !== null) {
			const current = useShellStore.getState().projectExtraRoots[projectId] ?? [];
			const next = dedupeRoots(roots);
			if (explicitScope !== null || JSON.stringify(current) !== JSON.stringify(next)) {
				useShellStore.getState().setProjectExtraRoots(projectId, next, scope);
			}
		}
		setPayload(committed);
	};
	useEffect(() => {
		registerBeforeNext(commit);
		return () => registerBeforeNext(null);
	});

	const openFolder = async () => {
		try {
			const path = await openDialog({ directory: true, multiple: false });
			if (typeof path === 'string' && path.length > 0) {
				setPicked((prev) => (prev.includes(path) ? prev : [path, ...prev]));
				setPicks(path);
				setDraftRoots(null);
			}
		} catch {
			/* dialog dismissed / unavailable */
		}
	};

	const choose = (value: string) => {
		setPicks(value);
		setDraftRoots(null);
	};

	return (
		<div className="mx-auto max-w-6xl" data-testid="project-body">
			<div className="mb-6 flex items-end justify-between gap-6">
				<div>
					<h1 className="font-display text-3xl font-bold leading-tight tracking-tight">
						Open a project.
					</h1>
					<p className="mt-2 max-w-[60ch] text-sm" style={{ color: 'var(--fg-muted)' }}>
						Project is the container. Every surface in the shell — files, artifacts, sessions,
						equipment, automations — is scoped to the one that is open.
					</p>
				</div>
				<SettingsScopeSwitch
					scope={scope}
					onScopeChange={setScope}
					projectAvailable={!!scopeRoot}
					ariaLabel="Scope"
					className="flex-none"
				/>
			</div>

			<div className="grid gap-10 lg:grid-cols-[1.3fr_1fr]">
				{/* ── Col A ───────────────────────────────────────────── */}
				<div>
					<Group
						title="Detected repositories"
						count={scanning ? 'scanning…' : `${candidates.length} found`}
						footer={
							<>
								<Button
									variant="outline"
									size="sm"
									className="h-7 gap-1.5 text-xs"
									onClick={() => void openFolder()}
									data-testid="project-open-folder"
								>
									<FolderOpen className="h-3.5 w-3.5" />
									Open a folder…
								</Button>
								<span className="flex-1" />
								<span>
									Scanned from <span className="font-mono">~/.claude/projects/</span>
								</span>
							</>
						}
					>
						<div role="radiogroup" aria-label="Project" data-testid="project-detected">
							{candidates.map((c) => (
								<OptionRow
									key={c.path}
									selected={selection === c.path}
									onSelect={() => choose(c.path)}
									icon={<Folder className="h-4 w-4" />}
									name={c.name}
									sub={c.path}
									mono
									testId="project-option"
									right={
										<>
											{!c.verified && (
												<span
													className="rounded-full px-1.5 py-0.5 text-[9.5px] font-semibold uppercase tracking-wider"
													style={{ background: 'var(--bg-raised)', color: 'var(--warning)' }}
													title="Best-effort decode of the ~/.claude/projects/ slug — this folder could not be confirmed on disk"
												>
													guess
												</span>
											)}
											{c.sessions !== null && (
												<span className="font-mono text-[11px]" style={{ color: 'var(--fg-muted)' }}>
													{c.sessions} session{c.sessions === 1 ? '' : 's'}
												</span>
											)}
										</>
									}
								/>
							))}
							<OptionRow
								selected={isEmpty}
								onSelect={() => choose(EMPTY)}
								icon={<Box className="h-4 w-4" />}
								name="Start empty"
								sub="Decide later. Nothing is scanned or written."
								testId="project-option-empty"
							/>
						</div>
					</Group>

					<div
						className="mt-4 rounded-md border"
						style={{ borderColor: 'var(--border)' }}
						data-testid="project-extra-roots"
					>
						<button
							type="button"
							onClick={() => setRootsOpen((o) => !o)}
							aria-expanded={rootsOpen}
							className="flex w-full items-center gap-2 px-3 py-2 text-left text-xs"
							style={{ color: 'var(--fg-muted)' }}
						>
							<ChevronRight
								className={cn('h-3.5 w-3.5 transition-transform', rootsOpen && 'rotate-90')}
							/>
							<span>Extra roots — folders outside the project the shell may read</span>
							<span className="ml-auto font-mono">{roots.length}</span>
						</button>
						{rootsOpen && (
							<div className="border-t px-3 py-3" style={{ borderColor: 'var(--border-soft)' }}>
								<p className="text-xs" style={{ color: 'var(--fg-muted)' }}>
									Adding a root lets the shell read and watch files there. It does not add the
									folder to the Explorer.
								</p>
								{roots.length > 0 && (
									<ul className="mt-3 grid gap-1" data-testid="project-roots-list">
										{roots.map((r) => (
											<li
												key={r}
												className="flex items-center gap-2 rounded-sm px-2 py-1"
												style={{ background: 'var(--bg-surface)' }}
												data-testid="root-row"
												data-path={r}
											>
												<span className="min-w-0 flex-1 truncate font-mono text-[12px]" title={r}>
													{r}
												</span>
												<Button
													variant="ghost"
													size="sm"
													className="h-6 px-2 text-[11px]"
													onClick={() => removeRoot(r)}
													data-testid="root-remove"
												>
													Remove
												</Button>
											</li>
										))}
									</ul>
								)}
								<label
									htmlFor="project-root-input"
									className="mt-3 block text-[11px] font-semibold"
									style={{ color: 'var(--fg-muted)' }}
								>
									Path
								</label>
								<Input
									id="project-root-input"
									placeholder="~/Documents/notes"
									value={rootInput}
									onChange={(e) => setRootInput(e.target.value)}
									onKeyDown={(e) => {
										if (e.key === 'Enter') {
											e.preventDefault();
											addRoot(rootInput);
											setRootInput('');
										}
									}}
									className="mt-1 font-mono text-xs"
									data-testid="project-roots-input"
								/>
								<Button
									variant="outline"
									size="sm"
									className="mt-2 h-7 text-xs"
									disabled={!rootInput.trim()}
									onClick={() => {
										addRoot(rootInput);
										setRootInput('');
									}}
									data-testid="project-roots-add"
								>
									Add root
								</Button>
							</div>
						)}
					</div>

					<WritesNote
						stepId="project"
						file={scopeFile}
						onOpenFile={
							scope === 'personal' || scopeProjectId
								? () => void openSettingsFile(scope, scope === 'project' ? scopeProjectId : null).catch(() => {})
								: undefined
						}
					/>
				</div>

				{/* ── Col B ───────────────────────────────────────────── */}
				<div className="grid content-start gap-4">
					<Group
						title="What was detected inside"
						count={isEmpty ? '—' : (selected?.name ?? baseName(selection))}
						footer={
							isEmpty ? undefined : (
								<>
									<FileText className="h-3.5 w-3.5" />
									<span className="font-mono">
										{selected?.name ?? baseName(selection)}/.claude/
									</span>
								</>
							)
						}
					>
						<div data-testid="project-inside">
							{isEmpty ? (
								<p className="p-3 text-xs" style={{ color: 'var(--fg-muted)' }}>
									An empty workspace. You can open a folder at any time with{' '}
									{labelFor('palette.projects')}.
								</p>
							) : insideLoading || !insideCounts ? (
								<p className="p-3 text-xs" style={{ color: 'var(--fg-muted)' }}>
									{insideLoading ? 'Scanning…' : 'Nothing could be read from this folder.'}
								</p>
							) : (
								<>
									<Kv name="skills" value={insideCounts.skills} />
									<Kv name="agents" value={insideCounts.agents} />
									<Kv name="hooks" value={insideCounts.hooks} />
									<Kv name="MCP tools" value={insideCounts.mcp} />
									<Kv name="commands" value={insideCounts.commands} />
								</>
							)}
						</div>
					</Group>

					<Group title="Scope">
						<p className="p-3 text-xs" style={{ color: 'var(--fg-muted)' }} data-testid="project-scope-note">
							Settings you change here are written to{' '}
							<span className="font-mono" style={{ color: 'var(--fg)' }}>
								{scopeFile}
							</span>
							. Project scope is committed with the repo and shared with whoever works in it;
							personal scope stays on this machine.
						</p>
					</Group>
				</div>
			</div>

			<div className="mt-8 flex items-center justify-end gap-3">
				<Button onClick={onContinue} data-testid="project-inline-continue">
					Continue
				</Button>
			</div>
		</div>
	);
}

// `designs/onboarding.html` `.grp` — header (label + count), body, footer.
function Group({
	title,
	count,
	footer,
	children,
}: {
	title: string;
	count?: string;
	footer?: React.ReactNode;
	children: React.ReactNode;
}) {
	return (
		<div
			className="overflow-hidden rounded-md border"
			style={{ borderColor: 'var(--border)', background: 'var(--bg-surface)' }}
		>
			<div
				className="flex items-center gap-2 border-b px-3 py-2 text-[11px] font-semibold uppercase tracking-[0.06em]"
				style={{ borderColor: 'var(--border-soft)', color: 'var(--fg-muted)' }}
			>
				<span>{title}</span>
				{count && (
					<span className="ml-auto font-mono normal-case tracking-normal" style={{ color: 'var(--fg-faint)' }}>
						{count}
					</span>
				)}
			</div>
			<div>{children}</div>
			{footer && (
				<div
					className="flex items-center gap-2 border-t px-3 py-2 text-[11px]"
					style={{ borderColor: 'var(--border-soft)', color: 'var(--fg-faint)' }}
				>
					{footer}
				</div>
			)}
		</div>
	);
}

// `designs/onboarding.html` `.orow` with radio semantics.
function OptionRow({
	selected,
	onSelect,
	icon,
	name,
	sub,
	mono,
	right,
	testId,
}: {
	selected: boolean;
	onSelect: () => void;
	icon: React.ReactNode;
	name: string;
	sub: string;
	mono?: boolean;
	right?: React.ReactNode;
	testId: string;
}) {
	return (
		<button
			type="button"
			role="radio"
			aria-checked={selected}
			onClick={onSelect}
			data-testid={testId}
			data-selected={selected}
			className="flex w-full items-center gap-3 border-b px-3 py-2.5 text-left transition-colors last:border-b-0 hover:bg-[var(--bg-raised)] focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-primary"
			style={{
				borderColor: 'var(--border-soft)',
				background: selected ? 'var(--bg-raised)' : undefined,
				boxShadow: selected ? 'inset 2px 0 0 var(--primary)' : undefined,
			}}
		>
			<span
				className="flex h-3.5 w-3.5 flex-none items-center justify-center rounded-full border"
				style={{ borderColor: selected ? 'var(--primary)' : 'var(--border-strong)' }}
				aria-hidden="true"
			>
				{selected && <span className="h-1.5 w-1.5 rounded-full" style={{ background: 'var(--primary)' }} />}
			</span>
			<span className="flex-none" style={{ color: 'var(--fg-muted)' }} aria-hidden="true">
				{icon}
			</span>
			<span className="min-w-0 flex-1">
				<span className="block truncate text-[13px] font-semibold">{name}</span>
				<span
					className={cn('block truncate text-[11px]', mono && 'font-mono')}
					style={{ color: 'var(--fg-muted)' }}
					title={sub}
				>
					{sub}
				</span>
			</span>
			{right && <span className="flex flex-none items-center gap-2">{right}</span>}
		</button>
	);
}

function Kv({ name, value }: { name: string; value: number }) {
	return (
		<div
			className="flex items-center justify-between border-b px-3 py-1.5 text-xs last:border-b-0"
			style={{ borderColor: 'var(--border-soft)' }}
		>
			<span style={{ color: 'var(--fg-muted)' }}>{name}</span>
			<span className="font-mono font-semibold">{value}</span>
		</div>
	);
}
