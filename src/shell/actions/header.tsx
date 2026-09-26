// D-06 header (WP-57): file path, Personal / Project scope, "Brief a Chi to
// make an action", Open file, `⋯` Reset this tab. Shared across all four
// tabs (Actions · Editor · Menus · Keys) — what "Reset this tab" clears
// depends on the active tab (G-ACTIONS §1.4/§1.5: the Actions tab deletes
// `actions`, the Menus tab deletes `menus`, the Keys tab deletes `bindings`;
// never writes a default back).

import { Check, Copy, Download, Ellipsis, ExternalLink, FileText, RotateCcw, Sparkles } from 'lucide-react';
import { useState } from 'react';
import { useNavigate } from '@tanstack/react-router';

import { Button } from '@/components/ui/button';
import {
	DropdownMenu,
	DropdownMenuContent,
	DropdownMenuItem,
	DropdownMenuSeparator,
	DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { confirm as confirmDialog, message as messageDialog } from '@/lib/transport/dialog-shim';
import { openActionsFile } from '@/lib/actions/client';
import {
	ActionsFileNotWritableError,
	ActionsValidationError,
	type ActionsScope,
	type EffectiveModel,
	LowerScopeOverrideError,
	resetKeybindings,
	resetMenuOverrides,
	resetUserActions,
} from '@/lib/actions/store';
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

/** The Keys tab writes `keybindings.json`, not `actions.json` (§1.1). */
function keybindingsPathLabel(scope: ActionsScope, projectRoot: string | null): string {
	return actionsPathLabel(scope, projectRoot).replace(/actions\.json$/, 'keybindings.json');
}

/** Blocker 2: every write in this header goes through this instead of a bare
 *  `.catch(() => {})` — `ActionsFileNotWritableError`, `ActionsValidationError`
 *  and `LowerScopeOverrideError` (all re-exported from `@/lib/actions/store`,
 *  never `registry`/`merge`) each carry a human-readable `.message`. */
function actionsErrorMessage(err: unknown): string {
	if (
		err instanceof ActionsFileNotWritableError ||
		err instanceof ActionsValidationError ||
		err instanceof LowerScopeOverrideError ||
		err instanceof Error
	) {
		return err.message;
	}
	return String(err);
}

const TAB_LABEL: Record<ActionsTabId, string> = {
	actions: 'Actions',
	editor: 'Editor',
	menus: 'Menus',
	keys: 'Keys',
	import: 'Import',
};

export interface ActionsHeaderProps {
	tab: ActionsTabId;
	scope: ActionsScope;
	onScopeChange: (scope: ActionsScope) => void;
	model: EffectiveModel;
}

export function ActionsHeader({ tab, scope, onScopeChange, model }: ActionsHeaderProps) {
	const navigate = useNavigate();
	const [copied, setCopied] = useState(false);
	const [writeError, setWriteError] = useState<string | null>(null);
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
				`command palette, a context menu, a key) — and write the entry for me.`
		);
	}

	async function handleOpenFile() {
		setWriteError(null);
		try {
			await openActionsFile('actions', scope, projectId);
		} catch (err) {
			setWriteError(actionsErrorMessage(err));
		}
	}

	function handleImport() {
		void navigate({ to: '/settings/actions/$tab', params: { tab: 'import' } });
	}

	async function handleExport() {
		// WP-61's own surface owns a real export; until then, say so rather
		// than silently doing nothing behind the menu item (blocker 2's spirit
		// — a write-shaped action always tells the user what happened).
		await messageDialog('Exporting a scope lands with the Import tab (WP-61). Nothing was written.', {
			title: 'Export this scope…',
			kind: 'info',
		});
	}

	async function handleResetTab() {
		const label = TAB_LABEL[tab];
		if (tab === 'actions') {
			const n = model.actions.filter((a) => a.source === scope).length;
			// §1.4 is authoritative over the D-06 mock's generic reset copy: this
			// deletes the scope's own authored actions — say exactly that, and
			// exactly how many, before the user confirms a delete.
			const ok = await confirmDialog(
				`This deletes the ${n} action${n === 1 ? '' : 's'} you've written at ${scope} scope from ${pathLabel}. Built-in and package actions are not affected.`,
				{ title: `Reset ${label}`, kind: 'warning', okLabel: `Delete ${n} action${n === 1 ? '' : 's'}` }
			);
			if (!ok) return;
			setWriteError(null);
			try {
				await resetUserActions(scope);
			} catch (err) {
				setWriteError(actionsErrorMessage(err));
			}
			return;
		}
		const file = tab === 'keys' ? keybindingsPathLabel(scope, model.projectRoot) : pathLabel;
		const ok = await confirmDialog(`Reset the ${label} tab at ${scope} scope? This rewrites ${file}.`, {
			title: `Reset ${label}`,
			kind: 'warning',
			okLabel: 'Reset',
		});
		if (!ok) return;
		setWriteError(null);
		try {
			if (tab === 'menus') await resetMenuOverrides(scope);
			else if (tab === 'keys') await resetKeybindings(scope);
		} catch (err) {
			setWriteError(actionsErrorMessage(err));
		}
	}

	return (
		<div className="vhead-wrap">
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
					<Button variant="outline" size="sm" className="min-h-[var(--btn-h-sm)] gap-1.5 text-xs" onClick={handleBriefChi}>
						<Sparkles className="h-3 w-3" />
						Brief a Chi to make an action
					</Button>
					<Button variant="outline" size="sm" className="min-h-[var(--btn-h-sm)] gap-1.5 text-xs" onClick={() => void handleOpenFile()}>
						<ExternalLink className="h-3 w-3" />
						Open file
					</Button>
					<DropdownMenu>
						<DropdownMenuTrigger asChild>
							<Button variant="ghost" size="icon" className="min-h-[var(--btn-h-sm)] min-w-[var(--btn-h-sm)]" aria-label="More">
								<Ellipsis className="h-3.5 w-3.5" />
							</Button>
						</DropdownMenuTrigger>
						<DropdownMenuContent align="end" className="w-72">
							<DropdownMenuItem onSelect={() => handleImport()}>
								<Download className="mr-2 h-3.5 w-3.5" />
								Import actions…
							</DropdownMenuItem>
							<DropdownMenuItem onSelect={() => void handleExport()}>
								<FileText className="mr-2 h-3.5 w-3.5" />
								Export this scope…
							</DropdownMenuItem>
							<DropdownMenuSeparator />
							<DropdownMenuItem
								onSelect={(e) => {
									e.preventDefault();
									void handleCopy();
								}}
							>
								{copied ? (
									<Check className="mr-2 h-3.5 w-3.5" style={{ color: 'var(--success)' }} />
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
			{writeError && (
				<div className="vhead-error" role="alert">
					{writeError}
				</div>
			)}
		</div>
	);
}
