// D-06 header (WP-57): file path, Personal / Project scope, "Brief a Chi to
// make an action", Open file, `⋯` Reset this tab. Shared across all four
// tabs (Actions · Editor · Menus · Keys) — what "Reset this tab" clears
// depends on the active tab (G-ACTIONS §1.4/§1.5: the Actions tab deletes
// `actions`, the Menus tab deletes `menus`, the Keys tab deletes `bindings`;
// never writes a default back).

import { Check, Copy, Ellipsis, ExternalLink, FileText, RotateCcw, Sparkles } from 'lucide-react';
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
import { openActionsFile } from '@/lib/actions/client';
import type { EffectiveModel } from '@/lib/actions/merge';
import { resetKeybindings, resetMenuOverrides, resetUserActions } from '@/lib/actions/store';
import type { ActionsScope } from '@/lib/actions/types';
import { SettingsScopeSwitch } from '@/shell/settings/scope-switch';
import { handToChi } from '@/shell/companion/companion-store';
import type { ActionsTabId } from './types';

function fileBaseName(root: string | null): string {
	return root?.replace(/[/\\]+$/, '').split(/[\\/]/).pop() ?? 'project';
}

export function actionsPathLabel(scope: ActionsScope, projectRoot: string | null): string {
	if (scope === 'personal') return '~/.ikenga/actions.json';
	return `${fileBaseName(projectRoot)}/.ikenga/actions.json`;
}

const TAB_LABEL: Record<ActionsTabId, string> = {
	actions: 'Actions',
	editor: 'Editor',
	menus: 'Menus',
	keys: 'Keys',
};

export interface ActionsHeaderProps {
	tab: ActionsTabId;
	scope: ActionsScope;
	onScopeChange: (scope: ActionsScope) => void;
	model: EffectiveModel;
}

export function ActionsHeader({ tab, scope, onScopeChange, model }: ActionsHeaderProps) {
	const [copied, setCopied] = useState(false);
	const projectId = scope === 'project' ? model.projectId : null;
	const pathLabel = actionsPathLabel(scope, model.projectRoot);
	const iykeLine = `iyke actions list --scope ${scope}`;
	const canReset = tab === 'actions' || tab === 'menus' || tab === 'keys';

	async function handleCopy() {
		try {
			await navigator.clipboard.writeText(iykeLine);
			setCopied(true);
			setTimeout(() => setCopied(false), 1200);
		} catch {
			setCopied(false);
		}
	}

	function handleBriefChi() {
		const path = pathLabel;
		handToChi(
			`Add an action to ${path}: give it a name, what it should run (dispatch to me, a shell ` +
				`command, an iyke route, a skill or a workflow), and where it should appear (the ` +
				`command palette, a context menu, a key). I will write the entry for you.`
		);
	}

	async function handleOpenFile() {
		try {
			await openActionsFile('actions', scope, projectId);
		} catch {
			// The file layer surfaces the failure via the actions:// watcher / its
			// own toast path; nothing further to do here.
		}
	}

	async function handleResetTab() {
		const label = TAB_LABEL[tab];
		const ok = await confirmDialog(`Reset the ${label} tab at ${scope} scope? This rewrites ${pathLabel}.`, {
			title: 'Reset this tab',
			kind: 'warning',
		});
		if (!ok) return;
		if (tab === 'actions') await resetUserActions(scope).catch(() => {});
		else if (tab === 'menus') await resetMenuOverrides(scope).catch(() => {});
		else if (tab === 'keys') await resetKeybindings(scope).catch(() => {});
	}

	return (
		<div className="vhead">
			<h1>
				Actions, menus and keys <span className="newchip" title="Not in the shipped shell">new</span>
			</h1>
			<span className="filepath">{pathLabel}</span>
			<span className="rt">
				<SettingsScopeSwitch
					scope={scope}
					onScopeChange={onScopeChange}
					projectAvailable={!!model.projectRoot}
					ariaLabel="Actions scope"
				/>
				<Button variant="outline" size="sm" className="h-7 gap-1.5 text-xs" onClick={handleBriefChi}>
					<Sparkles className="h-3 w-3" />
					Brief a Chi to make an action
				</Button>
				<Button variant="outline" size="sm" className="h-7 gap-1.5 text-xs" onClick={() => void handleOpenFile()}>
					<ExternalLink className="h-3 w-3" />
					Open file
				</Button>
				<DropdownMenu>
					<DropdownMenuTrigger asChild>
						<Button variant="ghost" size="icon" className="h-7 w-7" aria-label="More">
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
								<span className="truncate font-mono text-[10px] text-muted-foreground">{iykeLine}</span>
							</div>
						</DropdownMenuItem>
						<DropdownMenuItem onSelect={() => void handleOpenFile()}>
							<FileText className="mr-2 h-3.5 w-3.5" />
							<div className="flex min-w-0 flex-col">
								<span>Open file</span>
								<span className="truncate font-mono text-[10px] text-muted-foreground">{pathLabel}</span>
							</div>
						</DropdownMenuItem>
						<DropdownMenuSeparator />
						<DropdownMenuItem variant="destructive" disabled={!canReset} onSelect={() => void handleResetTab()}>
							<RotateCcw className="mr-2 h-3.5 w-3.5" />
							Reset this tab
						</DropdownMenuItem>
					</DropdownMenuContent>
				</DropdownMenu>
			</span>
		</div>
	);
}
