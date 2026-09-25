import { Check, Copy, Ellipsis, ExternalLink, FileText, RotateCcw } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/button';
import {
	DropdownMenu,
	DropdownMenuContent,
	DropdownMenuItem,
	DropdownMenuSeparator,
	DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { confirm as confirmDialog } from '@/lib/transport/dialog-shim';
import { openSettingsFile, writeSettingsField } from '@/lib/settings/client';
import { PERSONAL_ONLY_FIELDS } from '@/lib/settings/types';
import type { SettingsWriteOptions } from '@/lib/settings/types';
import {
	type SettingsSectionId,
	type SettingsScopeId,
	settingsIykeLine,
	settingsSection,
} from '@/shell/settings/nav';
import { useSettingsSection } from '@/shell/settings/field';

const PERSONAL_ONLY_FIELD_SET = new Set<string>(PERSONAL_ONLY_FIELDS);

export function settingsPathLabel(scope: SettingsScopeId, projectRoot: string | null): string {
	if (scope === 'personal') return '~/.ikenga/settings.json';
	const base = projectRoot?.replace(/[/\\]+$/, '').split(/[\\/]/).pop() ?? 'project';
	return `${base}/.ikenga/settings.json`;
}

interface SettingsSectionHeaderProps {
	sectionId: SettingsSectionId;
	searchActive: boolean;
}

export function SettingsSectionHeader({ sectionId, searchActive }: SettingsSectionHeaderProps) {
	const { scope, setScope, projectId, projectRoot, overrides, refresh } = useSettingsSection();
	const section = settingsSection(sectionId);
	const pathLabel = settingsPathLabel(scope, projectRoot);
	const iykeLine = settingsIykeLine(sectionId, scope);
	const [copied, setCopied] = useState(false);
	// Matches handleReset's own skip conditions below, so "Reset section" is
	// enabled exactly when it would actually change something. Secrets,
	// Integrations and People's fields are all informational (search-only,
	// `field: null` — see nav.tsx), so they'd otherwise show an enabled Reset
	// that silently does nothing.
	const hasFields = section.fields.some(
		(meta) => meta.field !== null && meta.field !== 'workspace.onboarding'
	);
	// A Personal/Project switch is meaningful only when the section owns at
	// least one field that can actually carry a project override. Secrets,
	// Integrations and People have no schema fields at all; About's and
	// Storage's fields are every one of them personal-only
	// (drafts/settings-schema.md §2.1 — `about.updates.*`,
	// `storage.screenshotDirectory`), so switching to Project there changed
	// nothing. Kept separate from `hasFields` below: About/Storage still have
	// fields worth resetting, they just can't be project-scoped.
	const scopeless = !section.fields.some(
		(meta) => meta.field !== null && !PERSONAL_ONLY_FIELD_SET.has(meta.field)
	);

	async function handleCopy() {
		try {
			await navigator.clipboard.writeText(iykeLine);
			setCopied(true);
			setTimeout(() => setCopied(false), 1200);
		} catch {
			setCopied(false);
		}
	}

	async function handleReset() {
		const ok = await confirmDialog(
			`Reset every field in ${section.label} to its default and rewrite ${pathLabel}? Other sections are untouched.`,
			{ title: 'Reset this section', kind: 'warning' }
		);
		if (!ok) return;
		for (const meta of section.fields) {
			if (!meta.field || meta.field === 'workspace.onboarding') continue;
			if (scope === 'project' && !overrides.has(meta.field)) continue;
			try {
				// `value` is ignored by the backend when `remove` is true; only the
				// `scope`/`projectId` pairing needs to satisfy SettingsWriteOptions'
				// discriminated union here (see field.tsx's useRevertOverride).
				if (scope === 'project') {
					if (!projectId) continue;
					await writeSettingsField({
						scope: 'project',
						field: meta.field,
						value: null,
						remove: true,
						projectId,
					} as SettingsWriteOptions);
				} else {
					await writeSettingsField({
						scope: 'personal',
						field: meta.field,
						value: null,
						remove: true,
					} as SettingsWriteOptions);
				}
			} catch {
				break;
			}
		}
		refresh();
	}

	return (
		<div className="sticky top-0 z-10 flex h-14 shrink-0 items-center gap-3 border-b border-border-soft bg-[var(--bg-base)] px-6">
			{searchActive ? (
				<div className="min-w-0">
					<div className="text-sm font-semibold text-foreground">Search settings</div>
					<div className="truncate font-mono text-[10px] text-muted-foreground">
						all nine sections
					</div>
				</div>
			) : (
				<div className="min-w-0">
					<div className="text-sm font-semibold text-foreground">{section.label}</div>
					<div className="flex min-w-0 items-center gap-2">
						<span className="truncate font-mono text-[10px] text-muted-foreground">{pathLabel}</span>
						<span className="shrink-0 font-mono text-[10px] text-muted-foreground">
							{scope === 'project' ? '· committed, shared with the team' : '· this machine, this user'}
						</span>
					</div>
				</div>
			)}

			{!searchActive && !scopeless && (
				// `min-h-[var(--tab-h)]` on each button, not a hardcoded px value —
				// `--tab-h` is the same tab-height token `.ccfg-tab` sizes off of
				// (src/shell/claude-config/claude-config.css) and is the shell's
				// 44px hit-target floor in spacious density (tokens.css
				// `[data-density='spacious']`; D-03 44px targets, WP-35 DoD).
				<div
					role="group"
					aria-label="Settings scope"
					className="ml-2 inline-flex items-center gap-0.5 rounded-md border border-border p-0.5"
				>
					<button
						type="button"
						aria-pressed={scope === 'personal'}
						onClick={() => setScope('personal')}
						className={`min-h-[var(--tab-h)] rounded px-2 py-1 text-xs transition-colors outline-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary ${
							scope === 'personal'
								? 'bg-card text-foreground shadow-sm'
								: 'text-muted-foreground hover:text-foreground'
						}`}
					>
						Personal
					</button>
					<button
						type="button"
						aria-pressed={scope === 'project'}
						disabled={!projectRoot}
						title={projectRoot ? undefined : 'The active project has no filesystem root'}
						onClick={() => setScope('project')}
						className={`min-h-[var(--tab-h)] rounded px-2 py-1 text-xs transition-colors outline-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary disabled:cursor-not-allowed disabled:opacity-50 ${
							scope === 'project'
								? 'bg-card text-foreground shadow-sm'
								: 'text-muted-foreground hover:text-foreground'
						}`}
					>
						Project
					</button>
				</div>
			)}

			<div className="ml-auto flex shrink-0 items-center gap-1">
				{scope === 'project' && overrides.size > 0 && !searchActive && (
					<span className="mr-1 font-mono text-[10px] text-primary">
						{overrides.size} override{overrides.size === 1 ? '' : 's'}
					</span>
				)}
				{!searchActive && (
					<Button
						variant="outline"
						size="sm"
						className="h-7 gap-1.5 text-xs"
						onClick={() => void openSettingsFile(scope, projectId).catch(() => {})}
					>
						<ExternalLink className="h-3 w-3" />
						Open file
					</Button>
				)}
				<DropdownMenu>
					<DropdownMenuTrigger asChild>
						<Button variant="ghost" size="icon" className="h-7 w-7" aria-label="Section menu">
							<Ellipsis className="h-3.5 w-3.5" />
						</Button>
					</DropdownMenuTrigger>
					<DropdownMenuContent align="end" className="w-72">
						<DropdownMenuItem
							onSelect={(e) => {
								e.preventDefault();
								void handleCopy();
							}}
						>
							{copied ? (
								<Check className="mr-2 h-3.5 w-3.5 text-emerald-600 dark:text-emerald-400" />
							) : (
								<Copy className="mr-2 h-3.5 w-3.5" />
							)}
							<div className="flex min-w-0 flex-col">
								<span>{copied ? 'Copied!' : 'Copy as iyke'}</span>
								<span className="truncate font-mono text-[10px] text-muted-foreground">
									{iykeLine}
								</span>
							</div>
						</DropdownMenuItem>
						<DropdownMenuItem
							onSelect={() => void openSettingsFile(scope, projectId).catch(() => {})}
						>
							<FileText className="mr-2 h-3.5 w-3.5" />
							<div className="flex min-w-0 flex-col">
								<span>Open file</span>
								<span className="truncate font-mono text-[10px] text-muted-foreground">
									{pathLabel}
								</span>
							</div>
						</DropdownMenuItem>
						<DropdownMenuSeparator />
						<DropdownMenuItem
							variant="destructive"
							disabled={!hasFields || searchActive}
							onSelect={() => void handleReset()}
						>
							<RotateCcw className="mr-2 h-3.5 w-3.5" />
							Reset section
						</DropdownMenuItem>
					</DropdownMenuContent>
				</DropdownMenu>
			</div>
		</div>
	);
}
