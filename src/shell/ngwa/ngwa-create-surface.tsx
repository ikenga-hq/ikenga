// Ngwa In-Shell Scaffolding Surface (WP-24 / locked D-02).
//
// 12-kind adaptive 4-question interview matching locked design D-02 (ngwa-create.png)
// with live tri-pane preview of destination tree, manifest.json, and head file.

import { useState, useId, useMemo } from 'react';
import {
	Zap,
	User,
	Terminal,
	ShieldCheck,
	RefreshCw,
	Clock,
	FileCode,
	AppWindow,
	Code2,
	Cpu,
	PlaySquare,
	Folder,
	Plus,
	Sparkles,
	Check,
	AlertCircle,
	FileText,
} from 'lucide-react';
import { pkgScaffold, type PkgScaffoldParams, type PkgScaffoldResult } from '@/lib/tauri-cmd';
import { useShellStore } from '@/lib/shell/shell-store';
import { useCompanionStore } from '@/shell/companion/companion-store';

export type EquipmentKind =
	| 'skill'
	| 'agent'
	| 'command'
	| 'hook'
	| 'workflow'
	| 'schedule'
	| 'artifact'
	| 'app'
	| 'tool'
	| 'engine'
	| 'sidecar'
	| 'project';

export interface KindDef {
	kind: EquipmentKind;
	name: string;
	desc: string;
	icon: React.ComponentType<{ className?: string }>;
	templateBadge: string;
	headFile: string;
	q2Title: string;
	q2Placeholder: string;
	q4Title: string;
	q4Desc: string;
	chips: string[];
	files: { name: string; role: string }[];
}

export const KIND_DEFS: KindDef[] = [
	{
		kind: 'artifact',
		name: 'Artifact',
		desc: 'One HTML page an agent writes and you read in a pane.',
		icon: FileCode,
		templateBadge: '_blueprints/artifact',
		headFile: 'index.html',
		q2Title: 'What does this artifact present?',
		q2Placeholder: 'Interactive dependency graph visualization mapping package imports and circular dependencies with zoomable canvas.',
		q4Title: 'What libraries or features does it use?',
		q4Desc: 'Injected script wrappers and presentation capabilities.',
		chips: ['iframe-sandbox', 'd3-charts', 'svg-export', 'dark-mode', 'tailwind-cdn'],
		files: [
			{ name: 'index.html', role: 'interactive HTML container' },
			{ name: 'style.css', role: 'optional scoped styles' },
		],
	},
	{
		kind: 'app',
		name: 'App',
		desc: 'An iframe or native-webview mini-app the shell mounts.',
		icon: AppWindow,
		templateBadge: '_templates/ui-iframe',
		headFile: 'manifest.json',
		q2Title: 'What does this mini-app provide?',
		q2Placeholder: 'Database inspector mini-app querying local SQLite tables with pagination, schema visualizer, and row editing.',
		q4Title: 'What permissions does it require?',
		q4Desc: 'Shell IPC and host capabilities granted by the user.',
		chips: ['db_query', 'fs_read', 'viewer_mount', 'clipboard', 'storage'],
		files: [
			{ name: 'manifest.json', role: 'id, kind, permissions, entry' },
			{ name: 'package.json', role: 'name, version, scripts' },
			{ name: 'index.html', role: 'shell mount target' },
			{ name: 'src/main.ts', role: 'app entrypoint' },
		],
	},
	{
		kind: 'tool',
		name: 'Tool (MCP)',
		desc: 'A headless MCP server exposing tools to every engine.',
		icon: Code2,
		templateBadge: '_templates/mcp',
		headFile: 'manifest.json',
		q2Title: 'What tools does this server provide?',
		q2Placeholder: 'Local Git MCP server providing git_status, git_diff, and git_log tools over stdio JSON-RPC.',
		q4Title: 'Transport & permissions',
		q4Desc: 'RPC transport and host capabilities requested by the server.',
		chips: ['stdio', 'http', 'shell_execute', 'fs_read', 'net'],
		files: [
			{ name: 'manifest.json', role: 'mcp config, transport, schema' },
			{ name: 'package.json', role: 'server dependencies' },
			{ name: 'src/index.ts', role: 'MCP server implementation' },
			{ name: 'README.md', role: 'tools reference and usage' },
		],
	},
	{
		kind: 'engine',
		name: 'Engine',
		desc: 'An adapter wrapping a CLI as an AI backend.',
		icon: Cpu,
		templateBadge: '_templates/engine',
		headFile: 'manifest.json',
		q2Title: 'What CLI or runtime does this engine wrap?',
		q2Placeholder: 'Custom LLM orchestrator streaming token events and tool calls from local vLLM or Ollama instance.',
		q4Title: 'Streaming capabilities',
		q4Desc: 'Protocol features supported by this engine adapter.',
		chips: ['streaming', 'tool-calls', 'json-logs', 'context-window', 'cancellation'],
		files: [
			{ name: 'manifest.json', role: 'engine id, binary, capabilities' },
			{ name: 'adapter.ts', role: 'CLI process wrapper' },
			{ name: 'README.md', role: 'setup instructions' },
		],
	},
	{
		kind: 'sidecar',
		name: 'Sidecar',
		desc: 'A supervised background process with restart-on-save.',
		icon: PlaySquare,
		templateBadge: '_templates/sidecar',
		headFile: 'manifest.json',
		q2Title: 'What background daemon does it supervise?',
		q2Placeholder: 'Fast file-watcher and build sidecar that generates TypeScript declarations and notifies the shell on file change.',
		q4Title: 'Supervision & signals',
		q4Desc: 'Restart policy, health check, and IPC pipes.',
		chips: ['restart-on-save', 'health-probe', 'stdout-tail', 'auto-restart', 'ipc-pipe'],
		files: [
			{ name: 'manifest.json', role: 'daemon config, restart policy' },
			{ name: 'run.sh', role: 'start script' },
			{ name: 'package.json', role: 'dependencies' },
		],
	},
	{
		kind: 'skill',
		name: 'Skill',
		desc: 'Knowledge an engine loads on demand. No process, no UI.',
		icon: Zap,
		templateBadge: '_templates/skill',
		headFile: 'SKILL.md',
		q2Title: 'What does it know?',
		q2Placeholder: "Turn a range of merged PRs into release notes in this repo's voice: group by user-visible change, drop refactors, link each line to its PR.",
		q4Title: 'What does it need to read?',
		q4Desc: 'Declared as intent in the frontmatter. A skill never grants itself anything.',
		chips: ['Read', 'Grep', 'Glob', 'Bash', 'Write', 'Edit', 'WebFetch', 'Agent', 'Skill'],
		files: [
			{ name: 'manifest.json', role: 'id, kind, empty permissions' },
			{ name: 'package.json', role: 'name, version 0.1.0' },
			{ name: 'README.md', role: 'what it is, how to install' },
			{ name: 'SKILL.md', role: 'frontmatter + body' },
		],
	},
	{
		kind: 'agent',
		name: 'Agent',
		desc: 'A persona with its own prompt, tools and model tier.',
		icon: User,
		templateBadge: '_blueprints/agent',
		headFile: 'agent.md',
		q2Title: 'What is its role and persona?',
		q2Placeholder: 'Autonomous test runner that inspects modified files, executes relevant unit tests, and formats concise regression reports.',
		q4Title: 'What capabilities can it invoke?',
		q4Desc: 'Tools available to this persona during invocation.',
		chips: ['Read', 'Grep', 'Glob', 'Bash', 'Write', 'Edit', 'WebFetch', 'Mcp', 'Chi'],
		files: [
			{ name: 'agent.md', role: 'frontmatter, system prompt, tools' },
			{ name: 'instructions.md', role: 'supporting guidelines' },
		],
	},
	{
		kind: 'command',
		name: 'Command',
		desc: 'A slash command that expands into a prompt.',
		icon: Terminal,
		templateBadge: '_blueprints/command',
		headFile: 'command.md',
		q2Title: 'What prompt does this command expand to?',
		q2Placeholder: 'Generate git commit messages following Conventional Commits format based on git diff of staged files with concise bullet points.',
		q4Title: 'Recommended model tier',
		q4Desc: 'Default engine tier for expanding this command.',
		chips: ['claude-3-5-sonnet', 'claude-3-7-sonnet', 'gemini-2.5-pro', 'gpt-4o'],
		files: [
			{ name: 'command.md', role: 'slash prompt template + hints' },
		],
	},
	{
		kind: 'hook',
		name: 'Hook',
		desc: 'A script the engine runs at a lifecycle point.',
		icon: ShieldCheck,
		templateBadge: '_blueprints/hook',
		headFile: 'hook.sh',
		q2Title: 'When does it run and what does it do?',
		q2Placeholder: 'Lint staged TypeScript files with Biome before tool execution and format modified files automatically.',
		q4Title: 'Lifecycle event',
		q4Desc: 'Which engine event triggers this hook.',
		chips: ['PostToolUse', 'PreToolUse', 'SessionStart', 'SessionEnd'],
		files: [
			{ name: 'hook.sh', role: 'executable shell hook' },
		],
	},
	{
		kind: 'workflow',
		name: 'Workflow',
		desc: 'A sequence of phases, imported rather than invented.',
		icon: RefreshCw,
		templateBadge: '_blueprints/workflow',
		headFile: 'workflow.md',
		q2Title: 'What multi-step process does it guide?',
		q2Placeholder: 'Three-phase release verification: automated test pass, changelog draft compilation, and semver tag check.',
		q4Title: 'Workflow phases',
		q4Desc: 'Sequential phases defining the workflow graph.',
		chips: ['research', 'plan', 'build', 'verify', 'deploy'],
		files: [
			{ name: 'workflow.md', role: 'step markdown with auto-fences' },
		],
	},
	{
		kind: 'schedule',
		name: 'Schedule',
		desc: 'A cron entry that starts a run without you.',
		icon: Clock,
		templateBadge: '_blueprints/schedule',
		headFile: 'schedule.json',
		q2Title: 'What task runs on schedule?',
		q2Placeholder: 'Nightly health audit scanning dependencies for security advisories and outdated npm packages.',
		q4Title: 'Schedule cadence',
		q4Desc: 'Cron timing expression preset.',
		chips: ['daily 05:00', 'hourly', 'weekdays 09:00', 'weekly Mon', 'monthly 1st'],
		files: [
			{ name: 'schedule.json', role: 'cron config and prompt' },
		],
	},
	{
		kind: 'project',
		name: 'Project',
		desc: 'A new container with its own .claude and explorer.',
		icon: Folder,
		templateBadge: '_blueprints/project',
		headFile: 'CLAUDE.md',
		q2Title: 'What repository or project context does it set?',
		q2Placeholder: 'Frontend component library with Tailwind design tokens, Storybook stories, and Vitest test suite.',
		q4Title: 'Starter equipment',
		q4Desc: 'Initial configuration seeded inside the project container.',
		chips: ['claude-md', 'rules', 'agents', 'skills'],
		files: [
			{ name: 'CLAUDE.md', role: 'repo instructions & auto-fences' },
			{ name: '.claude/settings.json', role: 'project settings' },
		],
	},
];

export interface NgwaCreateSurfaceProps {
	initialKind?: string;
	initialScope?: string;
}

export function NgwaCreateSurface({ initialKind, initialScope }: NgwaCreateSurfaceProps) {
	const nameInputId = useId();
	const descInputId = useId();

	// Active project from Zustand store
	const projects = useShellStore((s) => s.projects);
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const activeProject = projects.find((p) => p.id === activeProjectId) ?? projects[0];
	const projectName = activeProject?.display_name || activeProject?.id || 'royalti-co';

	// Selected kind
	const [selectedKind, setSelectedKind] = useState<EquipmentKind>(() => {
		const match = KIND_DEFS.find((d) => d.kind === initialKind);
		return match ? match.kind : 'skill';
	});

	const activeDef = useMemo(
		() => KIND_DEFS.find((d) => d.kind === selectedKind) ?? KIND_DEFS[5],
		[selectedKind]
	);

	// Form values
	const [slug, setSlug] = useState('release-notes');
	const [description, setDescription] = useState(
		activeDef.q2Placeholder
	);
	const [scope, setScope] = useState<'personal' | 'project'>(() => {
		return initialScope === 'personal' ? 'personal' : 'project';
	});
	const [selectedChips, setSelectedChips] = useState<string[]>(() =>
		activeDef.chips.slice(0, 3)
	);

	// Submission states
	const [isSubmitting, setIsSubmitting] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const [lastResult, setLastResult] = useState<PkgScaffoldResult | null>(null);

	// Validation
	const isSlugValid = useMemo(() => {
		return /^[a-z0-9]+(-[a-z0-9]+)*$/.test(slug.trim()) && slug.trim().length > 1;
	}, [slug]);

	const isDescValid = description.trim().length >= 20;
	const isValid = isSlugValid && isDescValid;

	// Update defaults when kind switches
	const handleKindChange = (newKind: EquipmentKind) => {
		setSelectedKind(newKind);
		const def = KIND_DEFS.find((d) => d.kind === newKind);
		if (def) {
			setDescription(def.q2Placeholder);
			setSelectedChips(def.chips.slice(0, 3));
		}
		setError(null);
	};

	const toggleChip = (chip: string) => {
		setSelectedChips((prev) =>
			prev.includes(chip) ? prev.filter((c) => c !== chip) : [...prev, chip]
		);
	};

	const pkgId = `io.royalti.${slug || 'unnamed'}`;
	const folderDisplay =
		scope === 'personal'
			? `~/.claude/${activeDef.kind === 'skill' ? 'skills' : activeDef.kind + 's'}/${slug || 'unnamed'}/`
			: `${projectName}/.claude/${activeDef.kind === 'skill' ? 'skills' : activeDef.kind + 's'}/${slug || 'unnamed'}/`;

	// Manifest preview JSON
	const manifestPreview = useMemo(() => {
		const obj = {
			id: pkgId,
			name: slug || 'unnamed',
			version: '0.1.0',
			ikenga_api: '1',
			kind: activeDef.kind,
			author: { name: 'Royalti', key: 'royalti' },
			permissions: { note: 'empty — a skill declares, never grants' },
		};
		return JSON.stringify(obj, null, 2);
	}, [pkgId, slug, activeDef.kind]);

	// Head file preview
	const headFilePreview = useMemo(() => {
		const descSnippet = description.trim().slice(0, 60) + (description.trim().length > 60 ? '...' : '');
		const toolsLine = selectedChips.join(', ');

		if (activeDef.headFile.endsWith('.html')) {
			return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <title>${slug}</title>
</head>
<body>
  <h1>${slug}</h1>
  <p>${descSnippet}</p>
  <!-- ikenga:auto:start body -->
  <div id="content">Interactive artifact content will render here.</div>
  <!-- ikenga:auto:end body -->
</body>
</html>`;
		}

		if (activeDef.headFile.endsWith('.sh')) {
			return `#!/usr/bin/env bash
# Hook: ${slug}
# ${descSnippet}

<!-- ikenga:auto:start body -->
echo "Running hook for ${slug}..."
<!-- ikenga:auto:end body -->`;
		}

		if (activeDef.headFile.endsWith('.json')) {
			return `{
  "id": "${slug}",
  "cadence": "${selectedChips[0] || 'daily 05:00'}",
  "description": "${descSnippet}"
}`;
		}

		return `---
name: ${slug}
description: ${descSnippet}
allowed-tools: ${toolsLine}
---

<!-- ikenga:auto:start body -->
A Chi fills this in when you brief it. Edit freely: it
only rewrites between the fences.
<!-- ikenga:auto:end body -->`;
	}, [slug, description, selectedChips, activeDef.headFile]);

	// Execution: Scaffold
	const handleScaffold = async (briefChi = false) => {
		if (!isValid || isSubmitting) return;

		setIsSubmitting(true);
		setError(null);

		const scopeParam = scope === 'personal' ? 'personal' : (activeProject?.id ? `project:${activeProject.id}` : 'workspace');

		const params: PkgScaffoldParams = {
			kind: activeDef.kind,
			name: slug,
			slug: slug.trim(),
			description: description.trim(),
			scope: scopeParam,
			projectId: scope === 'project' ? activeProject?.id : null,
			tools: selectedChips,
			authorName: 'Royalti',
			authorKey: 'royalti',
		};

		try {
			const result = await pkgScaffold(params);
			setLastResult(result);

			if (briefChi) {
				// WP-26: Brief a Chi dispatch handoff
				const briefText = `Brief for newly scaffolded ${activeDef.name} "${slug}":\n` +
					`Description: ${description.trim()}\n` +
					`Target folder: ${result.targetFolder}\n` +
					`Allowed tools / capabilities: ${selectedChips.join(', ')}\n\n` +
					`Please populate the body between the <!-- ikenga:auto --> fences according to this specification.`;

				useCompanionStore.getState().setDraft(briefText);
				useCompanionStore.getState().setState('expanded');
				useCompanionStore.getState().focusDispatch();
			}
		} catch (err: unknown) {
			const message = err instanceof Error ? err.message : String(err);
			setError(message);
		} finally {
			setIsSubmitting(false);
		}
	};

	return (
		<div className="ngwa-create-container flex flex-1 min-h-0 overflow-hidden" data-testid="ngwa-create-surface">
			{/* Subhead bar */}
			<div className="ngwa-create-subhead flex items-center justify-between px-4 py-2 border-b border-[var(--border-subtle)] bg-[var(--surface-base)] text-xs text-[var(--fg-muted)]">
				<div className="flex items-center gap-2">
					<span className="font-mono text-[var(--fg-base)]">/ngwa/create</span>
					<span>·</span>
					<span>12 kinds</span>
					<span>·</span>
					<span>scaffold-first</span>
				</div>
				<div className="flex items-center gap-3">
					<span>snapshot ngwa_snapshot · live</span>
				</div>
			</div>

			{/* 3-Column Work Area */}
			<div className="ngwa-create-workarea flex flex-1 min-h-0 overflow-hidden">
				{/* ── Left Column: 12 Kinds Picker ── */}
				<aside
					className="ngwa-create-kinds-col w-72 flex-shrink-0 border-r border-[var(--border-subtle)] bg-[var(--surface-base)] overflow-y-auto p-2 flex flex-col gap-1"
					aria-label="Equipment Kinds"
				>
					{KIND_DEFS.map((def) => {
						const Icon = def.icon;
						const isSelected = def.kind === selectedKind;
						return (
							<button
								key={def.kind}
								type="button"
								onClick={() => handleKindChange(def.kind)}
								className={`ngwa-create-kind-item flex items-start gap-3 p-2.5 rounded-md text-left transition-colors cursor-pointer border ${
									isSelected
										? 'bg-[var(--surface-raised)] border-[var(--border-focus)] text-[var(--fg-base)] shadow-sm'
										: 'border-transparent text-[var(--fg-muted)] hover:bg-[var(--surface-overlay)] hover:text-[var(--fg-base)]'
								}`}
								data-testid={`kind-select-${def.kind}`}
								aria-selected={isSelected}
							>
								<div
									className={`p-1.5 rounded flex items-center justify-center ${
										isSelected
											? 'bg-[var(--accent-subtle)] text-[var(--accent-fg)]'
											: 'bg-[var(--surface-sunken)] text-[var(--fg-muted)]'
									}`}
								>
									<Icon className="h-4 w-4" />
								</div>
								<div className="flex flex-col flex-1 min-w-0">
									<span className="text-xs font-medium text-[var(--fg-base)]">{def.name}</span>
									<span className="text-[11px] text-[var(--fg-muted)] leading-snug line-clamp-2 mt-0.5">
										{def.desc}
									</span>
								</div>
							</button>
						);
					})}
				</aside>

				{/* ── Middle Column: Adaptive 4-Question Interview ── */}
				<main className="ngwa-create-form-col flex-1 min-w-0 overflow-y-auto p-6 flex flex-col justify-between">
					<div className="space-y-6 max-w-2xl">
						{error && (
							<div className="p-3 rounded border border-[var(--color-danger-border)] bg-[var(--color-danger-subtle)] text-[var(--color-danger)] text-xs flex items-center gap-2">
								<AlertCircle className="h-4 w-4 flex-shrink-0" />
								<span>{error}</span>
							</div>
						)}

						{lastResult && (
							<div className="p-3 rounded border border-[var(--border-focus)] bg-[var(--surface-raised)] text-xs flex items-center gap-2" data-testid="scaffold-success-banner">
								<Check className="h-4 w-4 text-[var(--accent-fg)] flex-shrink-0" />
								<div className="flex-1">
									<span className="font-semibold text-[var(--fg-base)]">Scaffolded successfully: </span>
									<span className="text-[var(--fg-muted)]">
										Created {lastResult.filesWritten.length} files for {lastResult.kind} "{lastResult.slug}" in <code>{lastResult.targetFolder}</code>
									</span>
								</div>
							</div>
						)}

						{/* Question 1: Name / Slug */}
						<div className="space-y-2">
							<div className="flex items-baseline justify-between">
								<h2 className="text-sm font-semibold text-[var(--fg-base)] flex items-center gap-2">
									<span className="text-[var(--accent-fg)]">1</span> What is it called?
								</h2>
							</div>
							<p className="text-xs text-[var(--fg-muted)]">
								Becomes the folder name, the id and the slug in every template file.
							</p>
							<div className="relative flex items-center">
								<input
									id={nameInputId}
									type="text"
									value={slug}
									onChange={(e) => setSlug(e.target.value.toLowerCase().replace(/\s+/g, '-'))}
									placeholder="e.g. release-notes"
									className={`w-full px-3 py-2 text-xs font-mono rounded border bg-[var(--surface-input)] text-[var(--fg-base)] placeholder-[var(--fg-subtle)] focus:outline-none focus:ring-1 ${
										isSlugValid
											? 'border-[var(--border-subtle)] focus:border-[var(--border-focus)] focus:ring-[var(--border-focus)]'
											: 'border-[var(--color-danger)] focus:ring-[var(--color-danger)]'
									}`}
									data-testid="input-slug"
								/>
								<span className="absolute right-3 text-xs font-mono text-[var(--fg-subtle)] pointer-events-none select-none">
									{pkgId}
								</span>
							</div>
							{!isSlugValid && (
								<p className="text-[11px] text-[var(--color-danger)]">
									Slug must contain lowercase letters, numbers, and hyphens (e.g. <code>release-notes</code>).
								</p>
							)}
							<p className="text-[11px] text-[var(--fg-muted)]">
								Folder, id and every <code>{`{{slug}}`}</code> in the template resolve to {slug || 'unnamed'}.
							</p>
						</div>

						{/* Question 2: Description */}
						<div className="space-y-2">
							<h2 className="text-sm font-semibold text-[var(--fg-base)] flex items-center gap-2">
								<span className="text-[var(--accent-fg)]">2</span> {activeDef.q2Title}
							</h2>
							<p className="text-xs text-[var(--fg-muted)]">
								One paragraph. It becomes the description an engine matches against when deciding to load this {activeDef.kind}.
							</p>
							<textarea
								id={descInputId}
								rows={3}
								value={description}
								onChange={(e) => setDescription(e.target.value)}
								placeholder={activeDef.q2Placeholder}
								className={`w-full px-3 py-2 text-xs rounded border bg-[var(--surface-input)] text-[var(--fg-base)] placeholder-[var(--fg-subtle)] focus:outline-none focus:ring-1 resize-none ${
									isDescValid
										? 'border-[var(--border-subtle)] focus:border-[var(--border-focus)] focus:ring-[var(--border-focus)]'
										: 'border-[var(--color-warning)] focus:ring-[var(--color-warning)]'
								}`}
								data-testid="input-description"
							/>
							<div className="flex justify-between items-center text-[11px]">
								<span className={isDescValid ? 'text-[var(--fg-muted)]' : 'text-[var(--color-warning)]'}>
									{description.trim().length} characters — {isDescValid ? 'enough for an engine to match against.' : 'minimum 20 characters required.'}
								</span>
							</div>
						</div>

						{/* Question 3: Scope */}
						<div className="space-y-2">
							<h2 className="text-sm font-semibold text-[var(--fg-base)] flex items-center gap-2">
								<span className="text-[var(--accent-fg)]">3</span> Where does it live?
							</h2>
							<p className="text-xs text-[var(--fg-muted)]">
								Project scope keeps it with the repo and shadows any personal copy of the same name.
							</p>
							<div className="flex gap-3">
								<button
									type="button"
									onClick={() => setScope('personal')}
									className={`px-3 py-1.5 rounded text-xs border transition-colors cursor-pointer ${
										scope === 'personal'
											? 'border-[var(--border-focus)] bg-[var(--surface-raised)] text-[var(--fg-base)] font-medium'
											: 'border-[var(--border-subtle)] bg-[var(--surface-sunken)] text-[var(--fg-muted)] hover:text-[var(--fg-base)]'
									}`}
									data-testid="scope-personal-btn"
								>
									personal · ~/.claude
								</button>
								<button
									type="button"
									onClick={() => setScope('project')}
									className={`px-3 py-1.5 rounded text-xs border transition-colors cursor-pointer ${
										scope === 'project'
											? 'border-[var(--border-focus)] bg-[var(--surface-raised)] text-[var(--fg-base)] font-medium'
											: 'border-[var(--border-subtle)] bg-[var(--surface-sunken)] text-[var(--fg-muted)] hover:text-[var(--fg-base)]'
									}`}
									data-testid="scope-project-btn"
								>
									project · {projectName}
								</button>
							</div>
						</div>

						{/* Question 4: Capabilities / Chips */}
						<div className="space-y-2">
							<h2 className="text-sm font-semibold text-[var(--fg-base)] flex items-center gap-2">
								<span className="text-[var(--accent-fg)]">4</span> {activeDef.q4Title}
							</h2>
							<p className="text-xs text-[var(--fg-muted)]">
								{activeDef.q4Desc}
							</p>
							<div className="flex flex-wrap gap-2 pt-1">
								{activeDef.chips.map((chip) => {
									const isChipSelected = selectedChips.includes(chip);
									return (
										<button
											key={chip}
											type="button"
											onClick={() => toggleChip(chip)}
											className={`px-2.5 py-1 rounded text-xs font-mono border transition-colors cursor-pointer flex items-center gap-1.5 ${
												isChipSelected
													? 'border-[var(--accent-border)] bg-[var(--accent-subtle)] text-[var(--accent-fg)] font-medium'
													: 'border-[var(--border-subtle)] bg-[var(--surface-sunken)] text-[var(--fg-muted)] hover:border-[var(--border-base)] hover:text-[var(--fg-base)]'
											}`}
											data-testid={`chip-${chip}`}
										>
											{isChipSelected && <Check className="h-3 w-3" />}
											<span>{chip}</span>
										</button>
									);
								})}
							</div>
						</div>
					</div>

					{/* Action Bar */}
					<div className="pt-6 border-t border-[var(--border-subtle)] mt-8 flex flex-col gap-3">
						<div className="flex items-center gap-3">
							<button
								type="button"
								disabled={!isValid || isSubmitting}
								onClick={() => handleScaffold(false)}
								className={`px-4 py-2 rounded text-xs font-medium flex items-center gap-2 transition-colors cursor-pointer ${
									isValid && !isSubmitting
										? 'bg-[var(--accent-primary)] hover:bg-[var(--accent-primary-hover)] text-white shadow-sm'
										: 'bg-[var(--surface-disabled)] text-[var(--fg-disabled)] cursor-not-allowed opacity-60'
								}`}
								data-testid="scaffold-submit-btn"
							>
								<Plus className="h-3.5 w-3.5" />
								<span>{isSubmitting ? 'Scaffolding...' : 'Scaffold'}</span>
							</button>

							<button
								type="button"
								disabled={!isValid || isSubmitting}
								onClick={() => handleScaffold(true)}
								className={`px-4 py-2 rounded text-xs font-medium border flex items-center gap-2 transition-colors cursor-pointer ${
									isValid && !isSubmitting
										? 'border-[var(--border-base)] bg-[var(--surface-raised)] text-[var(--fg-base)] hover:bg-[var(--surface-overlay)]'
										: 'border-[var(--border-subtle)] text-[var(--fg-disabled)] cursor-not-allowed opacity-60'
								}`}
								data-testid="scaffold-brief-btn"
							>
								<Sparkles className="h-3.5 w-3.5 text-[var(--accent-fg)]" />
								<span>Scaffold + brief a Chi</span>
							</button>
						</div>

						<p className="text-[11px] text-[var(--fg-subtle)] leading-relaxed">
							Everything here is a folder you can edit; fenced regions are what a Chi may regenerate. Scaffold writes the files, registers them, and opens the tree in a pane.
						</p>
					</div>
				</main>

				{/* ── Right Column: Live Tri-Pane Preview ── */}
				<aside
					className="ngwa-create-preview-col w-96 flex-shrink-0 border-l border-[var(--border-subtle)] bg-[var(--surface-base)] flex flex-col overflow-hidden"
					aria-label="Live Template Preview"
					data-testid="ngwa-create-preview-col"
				>
					{/* Pane 1: File Tree */}
					<div className="flex-1 min-h-0 border-b border-[var(--border-subtle)] flex flex-col">
						<div className="flex items-center justify-between px-3 py-2 bg-[var(--surface-raised)] border-b border-[var(--border-subtle)] text-[11px]">
							<span className="font-semibold text-[var(--fg-base)] tracking-wider">SCAFFOLD</span>
							<span className="font-mono text-[var(--fg-subtle)]">{activeDef.templateBadge}</span>
						</div>
						<div className="p-3 overflow-y-auto font-mono text-[11px] space-y-1 text-[var(--fg-base)]">
							<div className="text-[var(--accent-fg)] font-medium truncate mb-2">
								{folderDisplay}
							</div>
							{activeDef.files.map((f) => (
								<div key={f.name} className="flex items-baseline justify-between gap-2 py-0.5">
									<span className="text-[var(--fg-base)] flex items-center gap-1.5 truncate">
										<FileText className="h-3 w-3 text-[var(--fg-subtle)] flex-shrink-0" />
										{f.name}
									</span>
									<span className="text-[10px] text-[var(--fg-subtle)] truncate text-right">
										{f.role}
									</span>
								</div>
							))}
						</div>
					</div>

					{/* Pane 2: Manifest JSON */}
					<div className="flex-1 min-h-0 border-b border-[var(--border-subtle)] flex flex-col">
						<div className="flex items-center justify-between px-3 py-2 bg-[var(--surface-raised)] border-b border-[var(--border-subtle)] text-[11px]">
							<span className="font-semibold text-[var(--fg-base)] tracking-wider">MANIFEST.JSON</span>
							<span className="font-mono text-[var(--fg-subtle)]">{`{{slug}} substituted`}</span>
						</div>
						<pre className="flex-1 min-h-0 p-3 overflow-y-auto font-mono text-[11px] text-[var(--fg-base)] bg-[var(--surface-sunken)] leading-tight whitespace-pre-wrap selection:bg-[var(--accent-subtle)]">
							{manifestPreview}
						</pre>
					</div>

					{/* Pane 3: Head File Preview */}
					<div className="flex-1 min-h-0 flex flex-col">
						<div className="flex items-center justify-between px-3 py-2 bg-[var(--surface-raised)] border-b border-[var(--border-subtle)] text-[11px]">
							<span className="font-semibold text-[var(--fg-base)] tracking-wider uppercase">
								{activeDef.headFile}
							</span>
							<span className="font-mono text-[var(--fg-subtle)]">head</span>
						</div>
						<pre className="flex-1 min-h-0 p-3 overflow-y-auto font-mono text-[11px] text-[var(--fg-base)] bg-[var(--surface-sunken)] leading-tight whitespace-pre-wrap selection:bg-[var(--accent-subtle)]">
							{headFilePreview}
						</pre>
					</div>
				</aside>
			</div>
		</div>
	);
}
