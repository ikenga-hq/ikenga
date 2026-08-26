// File-roots editor — surfaces the user-configurable FS allowlist (Rust
// side: `fs_roots`, persisted to `app_data_dir/fs_roots.json`). Imported by
// Settings → Storage. Inline-edit a path with Enter to commit / Esc to
// revert; commit calls `fsList` to verify the path is reachable and inside
// the allowlist before persisting.

import { useEffect, useState } from 'react';
import { open as openDialog } from '@/lib/transport/dialog-shim';
import { FolderOpen, FolderPlus, RotateCcw, Trash2 } from 'lucide-react';

import { Button } from '@/components/ui/button';
import { cn } from '@/components/ui/utils';
import { DEFAULT_FILE_ROOTS, useShellStore } from '@/lib/shell/shell-store';
import { fsList } from '@/lib/tauri-cmd';

interface EditablePathRowProps {
	value: string;
	onCommit: (next: string) => void;
	onRemove: () => void;
	removeLabel: string;
	isDefault?: boolean;
}

function EditablePathRow({
	value,
	onCommit,
	onRemove,
	removeLabel,
	isDefault,
}: EditablePathRowProps) {
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

export function FileRootsSectionBody() {
	const fileRoots = useShellStore((s) => s.fileRoots);
	const addFileRoot = useShellStore((s) => s.addFileRoot);
	const removeFileRoot = useShellStore((s) => s.removeFileRoot);
	const updateFileRoot = useShellStore((s) => s.updateFileRoot);
	const resetFileRoots = useShellStore((s) => s.resetFileRoots);

	async function handleAdd() {
		const picked = await openDialog({ directory: true, multiple: false });
		if (typeof picked === 'string') addFileRoot(picked);
	}

	return (
		<div className="space-y-3 px-4 py-3">
			<p className="text-xs text-muted-foreground">
				Directories the file browser and editor are allowed to open. Changes take effect immediately
				— the Rust resolver reads from the same list. Click any path to edit it; press{' '}
				<kbd>Enter</kbd> to commit, <kbd>Esc</kbd> to revert.
			</p>
			<ul className="space-y-1 rounded-md border border-border bg-background">
				{fileRoots.map((root) => {
					const isDefault = (DEFAULT_FILE_ROOTS as readonly string[]).includes(root);
					return (
						<EditablePathRow
							key={root}
							value={root}
							onCommit={(next) => updateFileRoot(root, next)}
							onRemove={() => removeFileRoot(root)}
							removeLabel={`Remove ${root}`}
							isDefault={isDefault}
						/>
					);
				})}
				{fileRoots.length === 0 && (
					<li className="px-3 py-3 text-xs text-muted-foreground">No file roots configured.</li>
				)}
			</ul>
			<div className="flex gap-2">
				<Button variant="outline" size="sm" onClick={handleAdd}>
					<FolderPlus className="mr-1 h-3.5 w-3.5" />
					Add directory
				</Button>
				<Button variant="ghost" size="sm" onClick={resetFileRoots}>
					<RotateCcw className="mr-1 h-3.5 w-3.5" />
					Reset to defaults
				</Button>
			</div>
		</div>
	);
}
