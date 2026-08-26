// Artifact creation wizard — single-screen variant.
//
// Picks project + archetype + folder + name, then:
//   1. Spawns `claude` in a terminal pane on the focused leaf.
//   2. Types the kickoff prompt into the PTY once it's ready, naming the
//      archetype + suggested file.
//   3. Watches the chosen folder for the first new `.html` and opens it in
//      a Studio loupe next to the terminal pane.
//
// The wizard closes as soon as the terminal is up — the terminal pane is
// the new surface; no separate success state.
//
// Mounted by:
//   - /projects/new-artifact   (deep-link route)
//   - command palette          ("New artifact…")
//   - workspace keybinding     (⌘⇧N)
//   - sidebar empty-state CTA  (consumes the same component)

import { useEffect, useMemo, useState } from 'react';
import { useNavigate } from '@tanstack/react-router';
import { open as openTauriDialog } from '@/lib/transport/dialog-shim';
import * as Icons from 'lucide-react';

import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';
import { cn } from '@/components/ui/utils';
import { useShellStore } from '@/lib/shell/shell-store';
import type { Project } from '@/lib/tauri-cmd';
import { AgentIcon } from '@/shell/artifact-wizard/agent-icon';
import {
	ARCHETYPES,
	type Archetype,
	type ArchetypeSlug,
	findArchetype,
} from '@/shell/artifact-wizard/archetypes';
import {
	loadLastAgent,
	loadLastAgentCustom,
	saveLastAgent,
	saveLastAgentCustom,
} from '@/shell/artifact-wizard/last-agent';
import { type AgentChoice, startArtifact } from '@/shell/artifact-wizard/scaffold';

type AgentKind = AgentChoice['kind'];

const AGENT_OPTIONS: { kind: Exclude<AgentKind, 'custom'>; label: string; hint?: string }[] = [
	{ kind: 'claude', label: 'claude', hint: 'CLI terminal' },
	{ kind: 'codex', label: 'codex', hint: 'CLI terminal' },
	{ kind: 'gemini', label: 'gemini', hint: 'CLI terminal' },
];

/** Map an onboarding-selected agent id to the wizard's agent kind. The
 *  onboarding step records the same well-known ids (`claude`, `codex`,
 *  `gemini`), so the mapping is identity for the known cases; anything
 *  unrecognised falls back to claude. */
function agentKindFromOnboarding(id: string | null): AgentKind {
	if (id === 'codex' || id === 'gemini' || id === 'claude') return id;
	return 'claude';
}

export interface ArtifactWizardProps {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	prefill?: {
		projectId?: string | null;
		archetypeSlug?: string | null;
		folder?: string | null;
		agent?: string | null;
	};
}

export function ArtifactWizard({ open, onOpenChange, prefill }: ArtifactWizardProps) {
	const projects = useShellStore((s) => s.projects);
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const onboardingAgentId = useShellStore((s) => s.onboarding.selectedAgentId);
	const navigate = useNavigate();

	const initialProjectId = useMemo(() => {
		const fromPrefill = prefill?.projectId ?? null;
		if (fromPrefill && projects.some((p) => p.id === fromPrefill)) return fromPrefill;
		return activeProjectId;
	}, [prefill?.projectId, projects, activeProjectId]);

	const initialAgentKind = useMemo<AgentKind>(() => {
		const fromPrefill = prefill?.agent ?? null;
		if (fromPrefill === 'claude' || fromPrefill === 'codex' || fromPrefill === 'gemini') {
			return fromPrefill;
		}
		if (fromPrefill === 'custom') return 'custom';
		return agentKindFromOnboarding(onboardingAgentId);
	}, [prefill?.agent, onboardingAgentId]);

	const [projectId, setProjectId] = useState<string>(initialProjectId);
	const [archetypeSlug, setArchetypeSlug] = useState<ArchetypeSlug | null>(
		(findArchetype(prefill?.archetypeSlug ?? null)?.slug as ArchetypeSlug | undefined) ?? null
	);
	const [name, setName] = useState<string>('');
	const [folder, setFolder] = useState<string>('');
	const [folderEdited, setFolderEdited] = useState(false);
	const [agentKind, setAgentKind] = useState<AgentKind>(initialAgentKind);
	const [customAgentCmd, setCustomAgentCmd] = useState<string>('');
	const [submitting, setSubmitting] = useState(false);
	const [error, setError] = useState<string | null>(null);

	const project = projects.find((p) => p.id === projectId) ?? null;
	const archetype = findArchetype(archetypeSlug);

	// Re-anchor whenever the wizard opens.
	useEffect(() => {
		if (!open) return;
		setProjectId(initialProjectId);
		const pa = findArchetype(prefill?.archetypeSlug ?? null);
		setArchetypeSlug(pa ? (pa.slug as ArchetypeSlug) : null);
		setAgentKind(initialAgentKind);
		setError(null);
		setSubmitting(false);
		if (prefill?.folder) {
			setFolder(prefill.folder);
			setFolderEdited(true);
		} else {
			setFolderEdited(false);
		}
	}, [open, initialProjectId, initialAgentKind, prefill?.archetypeSlug, prefill?.folder]);

	// Auto-derive folder from project root + archetype subdir until the
	// user types/picks something else.
	useEffect(() => {
		if (folderEdited) return;
		if (!project?.root_path || !archetype) {
			setFolder('');
			return;
		}
		setFolder(joinPath(project.root_path, archetype.defaultSubdir));
	}, [project?.root_path, archetype, folderEdited]);

	// Per-project last-agent memory: when the wizard opens (or the user
	// switches project mid-wizard), pre-select whatever agent was used last
	// time for that project. Skipped when the caller explicitly passed an
	// `?agent=` prefill — explicit deep-links win over memory.
	useEffect(() => {
		if (!open || !projectId) return;
		const fromPrefill = prefill?.agent ?? null;
		if (
			fromPrefill === 'claude' ||
			fromPrefill === 'codex' ||
			fromPrefill === 'gemini' ||
			fromPrefill === 'custom'
		) {
			return;
		}
		let cancelled = false;
		void loadLastAgent(projectId)
			.then(async (kind) => {
				if (cancelled || !kind) return;
				setAgentKind(kind);
				if (kind === 'custom') {
					const cmd = await loadLastAgentCustom(projectId);
					if (!cancelled && cmd) setCustomAgentCmd(cmd);
				}
			})
			.catch((e) => console.warn('[wizard] loadLastAgent failed:', e));
		return () => {
			cancelled = true;
		};
	}, [open, projectId, prefill?.agent]);

	function close() {
		onOpenChange(false);
	}

	async function pickFolder() {
		const picked = await openTauriDialog({
			directory: true,
			multiple: false,
			defaultPath: project?.root_path ?? undefined,
		}).catch(() => null);
		if (typeof picked === 'string' && picked.length > 0) {
			setFolder(picked);
			setFolderEdited(true);
		}
	}

	function resolveAgent(): AgentChoice | null {
		if (agentKind === 'custom') {
			const tokens = customAgentCmd.trim().split(/\s+/).filter(Boolean);
			if (tokens.length === 0) return null;
			return { kind: 'custom', cmd: tokens };
		}
		return { kind: agentKind };
	}

	async function submit() {
		if (!project) {
			setError('Pick a project.');
			return;
		}
		if (!archetype) {
			setError('Pick an archetype.');
			return;
		}
		if (name.trim().length === 0) {
			setError('Give the artifact a name.');
			return;
		}
		if (folder.trim().length === 0) {
			setError('Folder path can not be empty.');
			return;
		}
		const agent = resolveAgent();
		if (!agent) {
			setError('Custom agent needs a command — e.g. `/usr/local/bin/my-agent --flag`.');
			return;
		}
		setSubmitting(true);
		setError(null);
		try {
			await startArtifact({
				project,
				archetype,
				name: name.trim(),
				folder: folder.trim(),
				agent,
			});
			// Remember this agent for the next wizard run in this project.
			// Fire-and-forget — the wizard closes either way; a settings_kv
			// blip shouldn't block the user.
			void saveLastAgent(project.id, agent.kind).catch((e) =>
				console.warn('[wizard] saveLastAgent failed:', e)
			);
			if (agent.kind === 'custom') {
				void saveLastAgentCustom(project.id, customAgentCmd).catch((e) =>
					console.warn('[wizard] saveLastAgentCustom failed:', e)
				);
			}
			// Close immediately — the terminal pane is the new surface.
			close();
		} catch (e) {
			setError(e instanceof Error ? e.message : String(e));
		} finally {
			setSubmitting(false);
		}
	}

	const canSubmit =
		!!project &&
		!!archetype &&
		name.trim().length > 0 &&
		folder.trim().length > 0 &&
		(agentKind !== 'custom' || customAgentCmd.trim().length > 0);

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent
				className="sm:max-w-2xl"
				onEscapeKeyDown={(e) => {
					if (submitting) e.preventDefault();
				}}
			>
				<DialogHeader>
					<DialogTitle>New artifact</DialogTitle>
					<DialogDescription>
						Brief an agent in the project's context. A terminal pane opens with the chosen CLI
						running; the loupe lights up when the agent writes the file.
					</DialogDescription>
				</DialogHeader>

				<div className="flex flex-col gap-4">
					<ProjectField
						projects={projects}
						projectId={projectId}
						onPick={setProjectId}
						project={project}
						onAddProject={() => {
							close();
							void navigate({ to: '/settings/projects' });
						}}
					/>

					<ArchetypeGrid archetype={archetype} onPick={setArchetypeSlug} />

					<NameField name={name} onChange={setName} />

					<FolderField
						folder={folder}
						onChange={(v) => {
							setFolder(v);
							setFolderEdited(true);
						}}
						onPick={pickFolder}
						projectRoot={project?.root_path ?? null}
					/>

					<AgentField
						agentKind={agentKind}
						onAgentKindChange={setAgentKind}
						customAgentCmd={customAgentCmd}
						onCustomAgentCmdChange={setCustomAgentCmd}
					/>

					{error && (
						<div className="rounded border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive">
							{error}
						</div>
					)}
				</div>

				<DialogFooter className="mt-2">
					<Button variant="ghost" onClick={close} disabled={submitting}>
						Cancel
					</Button>
					<Button onClick={submit} disabled={submitting || !canSubmit}>
						{submitting ? 'Starting…' : 'Start'}
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}

// ─── Fields ──────────────────────────────────────────────────────────────

function ProjectField({
	projects,
	projectId,
	onPick,
	project,
	onAddProject,
}: {
	projects: Project[];
	projectId: string;
	onPick: (id: string) => void;
	project: Project | null;
	onAddProject: () => void;
}) {
	const usable = projects.filter((p) => p.archived_at == null);
	return (
		<div className="flex flex-col gap-1.5">
			<label className="text-xs font-medium" htmlFor="wizard-project">
				Project
			</label>
			<select
				id="wizard-project"
				className="rounded border border-border bg-background px-2 py-1.5 text-sm"
				value={projectId}
				onChange={(e) => {
					if (e.target.value === '__add__') {
						onAddProject();
						return;
					}
					onPick(e.target.value);
				}}
			>
				{usable.map((p) => (
					<option key={p.id} value={p.id}>
						{p.display_name}
						{p.root_path ? ` · ${shortenPath(p.root_path)}` : ' · no root'}
					</option>
				))}
				<option value="__add__">+ Add new project…</option>
			</select>
			{project && !project.root_path && (
				<div className="rounded border border-amber-500/40 bg-amber-500/10 px-2 py-1 text-[11px] text-amber-700 dark:text-amber-300">
					This project has no root path — the agent won't have project context. Set a root in
					Settings → Projects first.
				</div>
			)}
		</div>
	);
}

function ArchetypeGrid({
	archetype,
	onPick,
}: {
	archetype: Archetype | null;
	onPick: (slug: ArchetypeSlug) => void;
}) {
	return (
		<div className="flex flex-col gap-1.5">
			<label className="text-xs font-medium">Archetype</label>
			<div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
				{ARCHETYPES.map((a) => {
					const Glyph = resolveGlyph(a.glyphName);
					const active = archetype?.slug === a.slug;
					return (
						<button
							key={a.slug}
							type="button"
							onClick={() => onPick(a.slug)}
							className={cn(
								'flex flex-col items-start gap-1 rounded border px-3 py-2 text-left transition-colors',
								active
									? 'border-foreground bg-accent text-accent-foreground'
									: 'border-border bg-background hover:bg-muted/30'
							)}
						>
							<div className="flex items-center gap-1.5 text-sm font-medium">
								<Glyph className="h-4 w-4" />
								<span>{a.label}</span>
							</div>
							<p className="text-[10px] leading-snug text-muted-foreground">{a.description}</p>
						</button>
					);
				})}
			</div>
		</div>
	);
}

function NameField({ name, onChange }: { name: string; onChange: (v: string) => void }) {
	return (
		<div className="flex flex-col gap-1.5">
			<label className="text-xs font-medium" htmlFor="wizard-name">
				Name
			</label>
			<input
				id="wizard-name"
				className="rounded border border-border bg-background px-2 py-1.5 text-sm"
				value={name}
				onChange={(e) => onChange(e.target.value)}
				placeholder="Q3 dashboard"
			/>
			<p className="text-[10px] text-muted-foreground">
				The agent uses this as a starting point — it can rename if needed.
			</p>
		</div>
	);
}

function FolderField({
	folder,
	onChange,
	onPick,
	projectRoot,
}: {
	folder: string;
	onChange: (v: string) => void;
	onPick: () => void;
	projectRoot: string | null;
}) {
	const outsideProject =
		!!projectRoot && folder.length > 0 && !folder.startsWith(projectRoot.replace(/\/+$/, ''));
	return (
		<div className="flex flex-col gap-1.5">
			<label className="text-xs font-medium" htmlFor="wizard-folder">
				Folder
			</label>
			<div className="flex items-center gap-2">
				<input
					id="wizard-folder"
					className="flex-1 rounded border border-border bg-background px-2 py-1.5 font-mono text-xs"
					value={folder}
					onChange={(e) => onChange(e.target.value)}
					placeholder="/absolute/path"
				/>
				<Button variant="outline" size="sm" onClick={onPick}>
					Browse…
				</Button>
			</div>
			<p className="text-[10px] text-muted-foreground">
				Watched for the agent's first <code>.html</code>. Defaults to the archetype's subdir under
				the project root.
			</p>
			{outsideProject && (
				<div className="rounded border border-amber-500/40 bg-amber-500/10 px-2 py-1 text-[11px] text-amber-700 dark:text-amber-300">
					Folder is outside the project root — the agent won't have project context.
				</div>
			)}
		</div>
	);
}

function AgentField({
	agentKind,
	onAgentKindChange,
	customAgentCmd,
	onCustomAgentCmdChange,
}: {
	agentKind: AgentKind;
	onAgentKindChange: (k: AgentKind) => void;
	customAgentCmd: string;
	onCustomAgentCmdChange: (v: string) => void;
}) {
	return (
		<div className="flex flex-col gap-1.5">
			<label className="text-xs font-medium">Agent</label>
			<div className="flex flex-wrap gap-1.5">
				{AGENT_OPTIONS.map((opt) => (
					<button
						key={opt.kind}
						type="button"
						onClick={() => onAgentKindChange(opt.kind)}
						title={opt.hint}
						className={cn(
							'inline-flex items-center gap-1.5 rounded border px-2 py-1 text-xs',
							agentKind === opt.kind
								? 'border-foreground bg-accent text-accent-foreground'
								: 'border-border bg-background hover:bg-muted/30'
						)}
					>
						<AgentIcon kind={opt.kind} size={14} />
						{opt.label}
					</button>
				))}
				<button
					type="button"
					onClick={() => onAgentKindChange('custom')}
					title="custom argv"
					className={cn(
						'inline-flex items-center gap-1.5 rounded border px-2 py-1 text-xs',
						agentKind === 'custom'
							? 'border-foreground bg-accent text-accent-foreground'
							: 'border-border bg-background hover:bg-muted/30'
					)}
				>
					<AgentIcon kind="custom" size={14} />
					custom
				</button>
			</div>
			{agentKind === 'custom' && (
				<input
					className="rounded border border-border bg-background px-2 py-1.5 font-mono text-xs"
					placeholder="e.g. /usr/local/bin/my-agent --flag"
					value={customAgentCmd}
					onChange={(e) => onCustomAgentCmdChange(e.target.value)}
				/>
			)}
			<p className="text-[10px] text-muted-foreground">
				<code>claude</code> / <code>codex</code> / <code>gemini</code> spawn the CLI in a terminal
				at the project root (kickoff is auto-pasted via bracketed paste).
			</p>
		</div>
	);
}

// ─── Helpers ─────────────────────────────────────────────────────────────

function joinPath(folder: string, name: string): string {
	const cleaned = folder.replace(/\/+$/, '');
	return `${cleaned}/${name}`;
}

function shortenPath(p: string): string {
	if (p.length <= 36) return p;
	return `…${p.slice(p.length - 35)}`;
}

type LucideIcon = (typeof Icons)['Square'];

function resolveGlyph(name: string): LucideIcon {
	const map = Icons as unknown as Record<string, LucideIcon>;
	return map[name] ?? Icons.Square;
}
