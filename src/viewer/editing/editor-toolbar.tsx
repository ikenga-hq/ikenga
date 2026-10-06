// Generic editing toolbar for the shared text editor — the Markdown toolbar's
// Edit / dirty / Save row generalised (plans/file-editing Shape 1). Owns no
// state: the frame passes the document's mode and status plus the callbacks.

import { Check, Loader2, Lock, Pencil, Save, Undo2 } from 'lucide-react';
import { cn } from '@/components/ui/utils';
import type { SaveState } from './use-text-document';

interface EditorToolbarProps {
	mode: 'view' | 'edit';
	dirty: boolean;
	saveState: SaveState;
	/** Why Edit and Save are unavailable, or null. */
	blocked: string | null;
	/** A structured format saved without a parse check (JSON5). */
	unvalidated?: boolean;
	onEdit: () => void;
	onDone: () => void;
	onCancel: () => void;
	onSave: () => void;
	/** Format-specific controls (Markdown: bold/italic/…), shown in Edit. */
	extras?: React.ReactNode;
}

export function EditorToolbar({
	mode,
	dirty,
	saveState,
	blocked,
	unvalidated,
	onEdit,
	onDone,
	onCancel,
	onSave,
	extras,
}: EditorToolbarProps) {
	const editing = mode === 'edit';
	const saving = saveState.kind === 'saving';
	const canSave = dirty && !saving && blocked === null;
	return (
		<div
			data-state="editor-toolbar"
			className="flex shrink-0 items-center gap-1 border-b border-border bg-muted/20 px-3 py-1.5 text-xs"
		>
			{!editing ? (
				<>
					<ToolbarButton
						onClick={onEdit}
						label="Edit"
						disabled={blocked !== null}
						title={blocked ?? 'Edit this file'}
					>
						<Pencil className="h-3.5 w-3.5" />
						Edit
					</ToolbarButton>
					{blocked && (
						<span
							data-state="editor-blocked"
							className="ml-1 inline-flex min-w-0 items-center gap-1 truncate text-[11px] text-muted-foreground"
							title={blocked}
						>
							<Lock className="h-3 w-3 shrink-0" />
							<span className="truncate">{blocked}</span>
						</span>
					)}
				</>
			) : (
				<>
					<ToolbarButton
						onClick={onDone}
						label="Done"
						disabled={dirty || saving}
						title={dirty ? 'Save or cancel your changes first' : 'Back to view'}
					>
						<Check className="h-3.5 w-3.5" />
						Done
					</ToolbarButton>
					{extras}
					{unvalidated && (
						<span
							className="ml-1 text-[10px] text-muted-foreground"
							title="No parser for this format here"
						>
							not validated
						</span>
					)}
					<div className="ml-auto flex items-center gap-1">
						{dirty && (
							<span
								role="status"
								className="mr-1 h-1.5 w-1.5 rounded-full bg-amber-500"
								title="Unsaved changes"
								aria-label="Unsaved changes"
							/>
						)}
						<ToolbarButton
							onClick={onCancel}
							label="Cancel"
							disabled={saving}
							title="Discard your changes and reload the file"
						>
							<Undo2 className="h-3.5 w-3.5" />
							Cancel
						</ToolbarButton>
						<button
							type="button"
							onClick={onSave}
							disabled={!canSave}
							aria-label="Save"
							title={blocked ?? undefined}
							className={cn(
								'inline-flex items-center gap-1.5 rounded px-2 py-1 font-medium transition-colors',
								canSave
									? 'text-foreground hover:bg-muted'
									: 'cursor-not-allowed text-muted-foreground/50'
							)}
						>
							{saving ? (
								<Loader2 className="h-3.5 w-3.5 animate-spin" />
							) : (
								<Save className="h-3.5 w-3.5" />
							)}
							Save
						</button>
					</div>
				</>
			)}
		</div>
	);
}

function ToolbarButton({
	onClick,
	label,
	title,
	disabled,
	children,
}: {
	onClick: () => void;
	label: string;
	title?: string;
	disabled?: boolean;
	children: React.ReactNode;
}) {
	return (
		<button
			type="button"
			onClick={onClick}
			aria-label={label}
			title={title}
			disabled={disabled}
			className="inline-flex items-center gap-1.5 rounded px-2 py-1 font-medium text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent"
		>
			{children}
		</button>
	);
}
