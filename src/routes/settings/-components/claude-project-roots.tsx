// Project roots scanned by the /claude config browser. Moved out of the
// retired `legacy-sections.tsx` quarantine; rendered alongside file roots
// in Settings → Storage so all "directories Ikenga reads from" live in one
// place. Personal `~/.claude/` is always scanned in addition to whatever
// the user adds here — it doesn't need to be listed explicitly.

import { useEffect, useState } from 'react';
import { open as openDialog } from '@/lib/transport/dialog-shim';
import { FolderOpen, FolderPlus, Info, RotateCcw, Trash2 } from 'lucide-react';
import { Link } from '@tanstack/react-router';

import { Button } from '@/components/ui/button';
import { cn } from '@/components/ui/utils';
import { DEFAULT_CLAUDE_PROJECT_ROOTS, useShellStore } from '@/lib/shell/shell-store';
import { fsList } from '@/lib/tauri-cmd';

export function ClaudeProjectRootsSectionBody() {
	const claudeProjectRoots = useShellStore((s) => s.claudeProjectRoots);
	const addClaudeProjectRoot = useShellStore((s) => s.addClaudeProjectRoot);
	const removeClaudeProjectRoot = useShellStore((s) => s.removeClaudeProjectRoot);
	const updateClaudeProjectRoot = useShellStore((s) => s.updateClaudeProjectRoot);
	const resetClaudeProjectRoots = useShellStore((s) => s.resetClaudeProjectRoots);
	const claudeWatchEnabled = useShellStore((s) => s.claudeWatchEnabled);
	const setClaudeWatchEnabled = useShellStore((s) => s.setClaudeWatchEnabled);

	async function handleAdd() {
		const picked = await openDialog({ directory: true, multiple: false });
		if (typeof picked === 'string') addClaudeProjectRoot(picked);
	}

	return (
		<div className="space-y-3 px-4 py-3">
			<div className="flex items-start gap-2 rounded-md border-l-4 border-l-amber-400/70 bg-muted/50 px-3 py-2 text-xs">
				<Info className="mt-0.5 h-3.5 w-3.5 shrink-0 text-amber-600" />
				<div>
					<div className="font-medium text-foreground">Heads up.</div>
					<p className="text-muted-foreground">
						Project roots have been replaced by Projects (
						<Link
							to="/settings/projects"
							className="underline decoration-dotted underline-offset-2 hover:text-foreground"
						>
							Settings → Projects
						</Link>
						). Roots are auto-migrated to projects on first launch; this list is read-only and will
						be removed in a future release.
					</p>
				</div>
			</div>
			<p className="text-xs text-muted-foreground">
				Project roots scanned by the <code>/claude</code> config browser. Each root should contain a{' '}
				<code>.claude/</code> dir with agents/skills/commands. Personal <code>~/.claude/</code> is
				always scanned in addition to these — no need to add it.
			</p>
			<ul className="space-y-1 rounded-md border border-border bg-background">
				{claudeProjectRoots.map((root) => {
					const isDefault = (DEFAULT_CLAUDE_PROJECT_ROOTS as readonly string[]).includes(root);
					return (
						<EditableRow
							key={root}
							value={root}
							onCommit={(next) => updateClaudeProjectRoot(root, next)}
							onRemove={() => removeClaudeProjectRoot(root)}
							removeLabel={`Remove ${root}`}
							isDefault={isDefault}
						/>
					);
				})}
				{claudeProjectRoots.length === 0 && (
					<li className="px-3 py-3 text-xs text-muted-foreground">
						No project roots configured — only personal <code>~/.claude/</code> will be shown.
					</li>
				)}
			</ul>
			<div className="flex gap-2">
				<Button variant="outline" size="sm" onClick={handleAdd}>
					<FolderPlus className="mr-1 h-3.5 w-3.5" />
					Add project root
				</Button>
				<Button variant="ghost" size="sm" onClick={resetClaudeProjectRoots}>
					<RotateCcw className="mr-1 h-3.5 w-3.5" />
					Reset to defaults
				</Button>
			</div>
			<label className="flex items-center gap-2 text-xs text-muted-foreground">
				<input
					type="checkbox"
					checked={claudeWatchEnabled}
					onChange={(e) => setClaudeWatchEnabled(e.target.checked)}
					className="h-3.5 w-3.5"
				/>
				Live-reload on file changes (uses fs watcher)
			</label>
		</div>
	);
}

interface EditableRowProps {
	value: string;
	onCommit: (next: string) => void;
	onRemove: () => void;
	removeLabel: string;
	isDefault?: boolean;
}

function EditableRow({ value, onCommit, onRemove, removeLabel, isDefault }: EditableRowProps) {
	const [draft, setDraft] = useState(value);
	const [invalid, setInvalid] = useState(false);

	useEffect(() => {
		setDraft(value);
		setInvalid(false);
	}, [value]);

	async function commit() {
		const trimmed = draft.trim();
		if (!trimmed || trimmed === value) {
			setDraft(value);
			setInvalid(false);
			return;
		}
		try {
			await fsList(trimmed);
		} catch {
			setInvalid(true);
			return;
		}
		setInvalid(false);
		onCommit(trimmed);
	}

	return (
		<li className="flex items-center justify-between gap-2 border-b border-border px-3 py-2 text-sm last:border-b-0">
			<div className="flex min-w-0 flex-1 items-center gap-2">
				<FolderOpen className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
				<input
					value={draft}
					onChange={(e) => {
						setDraft(e.target.value);
						if (invalid) setInvalid(false);
					}}
					onBlur={() => void commit()}
					onKeyDown={(e) => {
						if (e.key === 'Enter') {
							e.preventDefault();
							(e.target as HTMLInputElement).blur();
						} else if (e.key === 'Escape') {
							e.preventDefault();
							setDraft(value);
							setInvalid(false);
							(e.target as HTMLInputElement).blur();
						}
					}}
					aria-invalid={invalid || undefined}
					className={cn(
						'min-w-0 flex-1 rounded border border-transparent bg-transparent px-1 py-0.5 font-mono text-xs text-foreground outline-none',
						'hover:border-border focus:border-ring focus:bg-background',
						invalid && 'border-destructive bg-destructive/5 focus:border-destructive'
					)}
					spellCheck={false}
				/>
				{isDefault && (
					<span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[10px] uppercase text-muted-foreground">
						default
					</span>
				)}
			</div>
			<Button
				variant="ghost"
				size="sm"
				onClick={onRemove}
				className="h-7 px-2 text-muted-foreground hover:text-red-700"
				aria-label={removeLabel}
			>
				<Trash2 className="h-3.5 w-3.5" />
			</Button>
		</li>
	);
}
