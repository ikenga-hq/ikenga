import { Copy, Ellipsis, ExternalLink, FileText, RotateCcw } from 'lucide-react';
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
import {
	type SettingsSectionId,
	type SettingsScopeId,
	settingsIykeLine,
	settingsSection,
} from '@/shell/settings/nav';
import { useSettingsSection } from '@/shell/settings/field';

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
	const scopeless = section.fields.length === 0;

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
			if (meta.field === 'workspace.onboarding') continue;
			if (scope === 'project' && !overrides.has(meta.field)) continue;
			try {
				await writeSettingsField({ scope, field: meta.field, value: null, remove: true, projectId });
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
				<div
					role="group"
					aria-label="Settings scope"
					className="ml-2 inline-flex items-center gap-0.5 rounded-md border border-border p-0.5"
				>
					<button
						type="button"
						aria-pressed={scope === 'personal'}
						onClick={() => setScope('personal')}
						className={`rounded px-2 py-1 text-xs transition-colors outline-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary ${
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
						className={`rounded px-2 py-1 text-xs transition-colors outline-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary disabled:cursor-not-allowed disabled:opacity-50 ${
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
						<DropdownMenuItem onSelect={() => void handleCopy()}>
							<Copy className="mr-2 h-3.5 w-3.5" />
							<div className="flex min-w-0 flex-col">
								<span>Copy as iyke</span>
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
							disabled={scopeless || searchActive}
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
