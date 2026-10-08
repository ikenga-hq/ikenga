import { AlertCircle, Info } from 'lucide-react';

// A compact "couldn't load" row for explorer sections. A failed read is not
// an empty list, so sections render this instead of their empty state (or
// above a partial list) — with the reason and a retry when there is one.

export function SectionErrorRow({
	message,
	detail,
	onRetry,
	actionLabel = 'Retry',
	tone = 'error',
}: {
	message: string;
	detail?: string;
	onRetry?: () => void;
	/** Label for the `onRetry` button (e.g. "Dismiss" for a notice). */
	actionLabel?: string;
	/** `info` is for a notice that isn't a failure (e.g. "saving resumed"). */
	tone?: 'error' | 'info';
}) {
	const Icon = tone === 'info' ? Info : AlertCircle;
	return (
		<div
			role={tone === 'info' ? 'status' : 'alert'}
			data-testid="explorer-section-error"
			data-tone={tone}
			className="flex items-start gap-1.5 px-2 py-1.5 text-xs"
			style={{ color: 'var(--fg-muted)' }}
		>
			<Icon
				aria-hidden
				className="mt-px h-3.5 w-3.5 shrink-0"
				style={{ color: tone === 'info' ? 'var(--fg-muted)' : 'var(--danger)' }}
			/>
			<span className="min-w-0 flex-1">
				<span style={{ color: 'var(--fg)' }}>{message}</span>
				{detail && (
					<span className="block truncate" title={detail}>
						{detail}
					</span>
				)}
			</span>
			{onRetry && (
				<button
					type="button"
					onClick={onRetry}
					className="shrink-0 underline-offset-2 hover:underline"
					style={{ color: 'var(--primary)' }}
				>
					{actionLabel}
				</button>
			)}
		</div>
	);
}

export function errorMessageOf(err: unknown): string {
	if (err instanceof Error) return err.message;
	return typeof err === 'string' ? err : String(err);
}
