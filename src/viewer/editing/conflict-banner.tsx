// The F3 conflict choice (plans/file-editing Shape 2): the file changed (or
// went missing) on disk since the editor loaded it. Nothing is written until
// the user picks — a save never silently overwrites a newer file.

import { useEffect, useState } from 'react';
import { AlertTriangle, FileX2, Loader2 } from 'lucide-react';
import { cn } from '@/components/ui/utils';
import type { Conflict } from './use-text-document';

interface ConflictBannerProps {
	conflict: Conflict;
	mine: string;
	busy: boolean;
	onKeepMine: () => void;
	onLoadTheirs: () => void;
	onDiscard: () => void;
}

export function ConflictBanner({
	conflict,
	mine,
	busy,
	onKeepMine,
	onLoadTheirs,
	onDiscard,
}: ConflictBannerProps) {
	const [showDiff, setShowDiff] = useState(false);

	if (conflict.kind === 'deleted') {
		return (
			<div
				role="alert"
				data-state="editor-conflict-deleted"
				className="flex flex-wrap items-center gap-2 border-b border-amber-500/40 bg-amber-500/10 px-4 py-1.5 text-[11px] text-amber-700 dark:text-amber-300"
			>
				<FileX2 className="h-3 w-3 shrink-0" />
				<span>This file was moved or deleted. Your changes have not been saved.</span>
				<BannerButton onClick={onKeepMine} disabled={busy}>
					Save anyway (recreate it)
				</BannerButton>
				<BannerButton onClick={onDiscard} disabled={busy}>
					Discard my changes
				</BannerButton>
			</div>
		);
	}

	return (
		<div
			role="alert"
			data-state="editor-conflict"
			className="flex flex-col border-b border-amber-500/40 bg-amber-500/10 text-[11px] text-amber-700 dark:text-amber-300"
		>
			<div className="flex flex-wrap items-center gap-2 px-4 py-1.5">
				<AlertTriangle className="h-3 w-3 shrink-0" />
				<span>This file changed on disk since you opened it. Nothing has been saved.</span>
				<BannerButton onClick={onKeepMine} disabled={busy}>
					Keep mine (overwrite)
				</BannerButton>
				<BannerButton onClick={onLoadTheirs} disabled={busy}>
					Load theirs (discard my changes)
				</BannerButton>
				<BannerButton onClick={() => setShowDiff((v) => !v)} aria-expanded={showDiff}>
					{showDiff ? 'Hide diff' : 'Show diff'}
				</BannerButton>
			</div>
			{showDiff && <DiffPanel theirs={conflict.theirs} mine={mine} />}
		</div>
	);
}

function BannerButton({
	onClick,
	disabled,
	children,
	...rest
}: {
	onClick: () => void;
	disabled?: boolean;
	children: React.ReactNode;
	'aria-expanded'?: boolean;
}) {
	return (
		<button
			type="button"
			onClick={onClick}
			disabled={disabled}
			className="rounded px-1.5 py-0.5 font-medium underline-offset-2 hover:underline disabled:cursor-not-allowed disabled:opacity-50"
			{...rest}
		>
			{children}
		</button>
	);
}

type DiffLine = { id: number; kind: 'same' | 'theirs' | 'mine'; text: string };

/** Unified line diff, the file on disk ("theirs") against the draft ("mine").
 *  `diff` (Myers) is loaded on first open. */
export function DiffPanel({ theirs, mine }: { theirs: string; mine: string }) {
	const [lines, setLines] = useState<DiffLine[] | null>(null);
	const [error, setError] = useState<string | null>(null);

	useEffect(() => {
		let cancelled = false;
		setLines(null);
		import('diff')
			.then(({ diffLines }) => {
				if (cancelled) return;
				const out: DiffLine[] = [];
				for (const part of diffLines(theirs, mine)) {
					const kind: DiffLine['kind'] = part.added ? 'mine' : part.removed ? 'theirs' : 'same';
					const body = part.value.endsWith('\n') ? part.value.slice(0, -1) : part.value;
					for (const text of body.split('\n')) out.push({ id: out.length, kind, text });
				}
				setLines(out);
			})
			.catch((err) => {
				if (!cancelled) setError(err instanceof Error ? err.message : String(err));
			});
		return () => {
			cancelled = true;
		};
	}, [theirs, mine]);

	return (
		<div
			data-state="editor-conflict-diff"
			className="max-h-64 overflow-auto border-t border-amber-500/30 bg-background font-mono text-[11px] text-foreground"
		>
			<div className="flex gap-4 border-b border-border px-3 py-1 text-[10px] text-muted-foreground">
				<span>
					<span className="text-destructive">−</span> on disk (theirs)
				</span>
				<span>
					<span className="text-emerald-600 dark:text-emerald-400">+</span> your edits (mine)
				</span>
			</div>
			{error ? (
				<div className="px-3 py-2 text-destructive">Couldn’t compute the diff: {error}</div>
			) : lines === null ? (
				<div className="flex items-center px-3 py-2 text-muted-foreground">
					<Loader2 className="mr-2 h-3 w-3 animate-spin" /> Comparing…
				</div>
			) : (
				<pre className="m-0 px-0 py-1">
					{lines.map((l) => (
						<div
							key={l.id}
							data-diff={l.kind}
							className={cn(
								'whitespace-pre-wrap px-3',
								l.kind === 'theirs' && 'bg-destructive/10 text-destructive',
								l.kind === 'mine' && 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300'
							)}
						>
							{l.kind === 'theirs' ? '− ' : l.kind === 'mine' ? '+ ' : '  '}
							{l.text}
						</div>
					))}
				</pre>
			)}
		</div>
	);
}
