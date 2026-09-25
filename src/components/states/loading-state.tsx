import { cn } from '@/components/ui/utils';

// D-07 `states` contact sheet — the loading variant. "Loading uses the ember
// pulse, never a spinner" is a hard rule (design-spec-D-03-07.md §D-07 +
// §Shared rules token contract): no `Loader2`/`.animate-spin` in this file,
// ever. There is deliberately no `action` prop — nothing to act on mid-flight.
//
// designs/system-flows.html?state=states — the "loading" cell.

export interface LoadingStateProps {
	heading: string;
	body?: React.ReactNode;
	fill?: boolean;
	className?: string;
	'data-state': string;
}

export function LoadingState({ heading, body, fill, className, ...rest }: LoadingStateProps) {
	return (
		<div
			{...rest}
			role="status"
			aria-live="polite"
			className={cn(
				'flex w-full flex-col items-center justify-center gap-2 p-6 text-center',
				fill ? 'h-full' : 'min-h-[168px]',
				className
			)}
		>
			<span className="ember-dots" aria-hidden="true">
				<i />
				<i />
				<i />
			</span>
			<div
				className="font-medium"
				style={{ fontSize: 'var(--text-caption)', color: 'var(--fg)' }}
			>
				{heading}
			</div>
			{body && (
				<div
					className="max-w-xs"
					style={{
						fontSize: 'var(--text-micro)',
						color: 'var(--fg-muted)',
						lineHeight: 'var(--lead-micro)',
					}}
				>
					{body}
				</div>
			)}
		</div>
	);
}
