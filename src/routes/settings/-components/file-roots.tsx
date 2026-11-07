// Project extra-roots editor — the active project's `extra_roots` (settings
// `projects.extraRoots`). Imported by Settings → Projects. Not the fs
// allowlist itself (Settings → Storage, `fs-allowlist.tsx`), though on the
// desktop app `setProjectExtraRoots` also allows each newly added root.
// Inline-edit a path with Enter to commit / Esc to revert; commit calls
// `fsList` to check the path is reachable before persisting.

import { useEffect, useState } from 'react';
import { open as openDialog } from '@/lib/transport/dialog-shim';
import { FolderOpen, FolderPlus, RotateCcw, Trash2 } from 'lucide-react';

import { Button } from '@/components/ui/button';
import { cn } from '@/components/ui/utils';
import { useShellStore } from '@/lib/shell/shell-store';
import { fsList } from '@/lib/tauri-cmd';

interface EditablePathRowProps {
	value: string;
	onCommit: (next: string) => void;
	onRemove: () => void;
	removeLabel: string;
}

function EditablePathRow({
	value,
	onCommit,
	onRemove,
	removeLabel,
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
					type="text"
					value={draft}
					onChange={(e) => setDraft(e.target.value)}
					onBlur={commit}
					onKeyDown={(e) => {
						if (e.key === 'Enter') {
							e.currentTarget.blur();
						} else if (e.key === 'Escape') {
							setDraft(value);
							setInvalid(false);
						}
					}}
					className={cn(
						'flex-1 bg-transparent font-mono text-xs text-foreground outline-none',
						invalid && 'text-red-600'
					)}
					aria-label="Edit file root path"
				/>
				{invalid && (
					<span className="text-[10px] text-red-600">Unreachable or invalid path</span>
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
	const activeProject = useShellStore((s) => s.activeProject);
	const setProjectExtraRoots = useShellStore((s) => s.setProjectExtraRoots);
	const roots = activeProject?.extra_roots ?? [];

	function addRoot(path: string) {
		const activeId = activeProject?.id || 'default';
		if (!roots.includes(path)) {
			setProjectExtraRoots(activeId, [...roots, path]);
		}
	}

	function removeRoot(path: string) {
		const activeId = activeProject?.id || 'default';
		setProjectExtraRoots(activeId, roots.filter((r) => r !== path));
	}

	function updateRoot(oldPath: string, nextPath: string) {
		const activeId = activeProject?.id || 'default';
		setProjectExtraRoots(activeId, roots.map((r) => (r === oldPath ? nextPath : r)));
	}

	function resetRoots() {
		const activeId = activeProject?.id || 'default';
		setProjectExtraRoots(activeId, []);
	}

	async function handleAdd() {
		const picked = await openDialog({ directory: true, multiple: false });
		if (typeof picked === 'string') addRoot(picked);
	}

	return (
		<div className="space-y-3 px-4 py-3">
			<p className="text-xs text-muted-foreground">
				Extra folders this project shows in the file browser. In the desktop app, adding one also
				adds it to Settings → Storage → Folders you can open; removing one here leaves that list as
				it is. Click any path to edit it; press <kbd>Enter</kbd> to commit, <kbd>Esc</kbd> to
				revert.
			</p>
			<ul className="space-y-1 rounded-md border border-border bg-background">
				{roots.map((root) => (
					<EditablePathRow
						key={root}
						value={root}
						onCommit={(next) => updateRoot(root, next)}
						onRemove={() => removeRoot(root)}
						removeLabel={`Remove ${root}`}
					/>
				))}
				{roots.length === 0 && (
					<li className="px-3 py-3 text-xs text-muted-foreground">No extra roots configured.</li>
				)}
			</ul>
			<div className="flex gap-2">
				<Button variant="outline" size="sm" onClick={handleAdd}>
					<FolderPlus className="mr-1 h-3.5 w-3.5" />
					Add directory
				</Button>
				<Button variant="ghost" size="sm" onClick={resetRoots}>
					<RotateCcw className="mr-1 h-3.5 w-3.5" />
					Reset to defaults
				</Button>
			</div>
		</div>
	);
}
