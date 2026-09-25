// D-08 pane chrome — the thin info strip that "appears only when relevant"
// under the merged address row. designs/pane-chrome.html `.infostrip`.

import { AlertTriangle, RefreshCw, X } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { cn } from '@/components/ui/utils';

interface ArtifactInfoStripProps {
	kind: 'changed' | 'stopped';
	onDismiss?: () => void;
	onRestart?: () => void;
}

export function ArtifactInfoStrip({ kind, onDismiss, onRestart }: ArtifactInfoStripProps) {
	const warn = kind === 'stopped';
	return (
		<div
			data-state={kind === 'stopped' ? 'artifact-stopped-strip' : 'artifact-changed-strip'}
			role="status"
			className={cn(
				'flex shrink-0 items-center gap-2 border-b px-3 py-1.5 text-xs',
				warn
					? 'border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-400'
					: 'border-border bg-muted/30 text-muted-foreground'
			)}
		>
			{warn ? (
				<AlertTriangle className="h-3.5 w-3.5 shrink-0" />
			) : (
				<RefreshCw className="h-3.5 w-3.5 shrink-0" />
			)}
			<span className="min-w-0 flex-1 truncate">
				{warn
					? 'viewer server stopped — the artifact on disk is untouched'
					: 'changed on disk — reloaded'}
			</span>
			{warn && onRestart && (
				<Button size="sm" variant="outline" className="h-6 px-2 text-xs" onClick={onRestart}>
					Restart
				</Button>
			)}
			{onDismiss && (
				<button
					type="button"
					onClick={onDismiss}
					aria-label="Dismiss notice"
					className="text-muted-foreground hover:text-foreground"
				>
					<X className="h-3.5 w-3.5" />
				</button>
			)}
		</div>
	);
}
