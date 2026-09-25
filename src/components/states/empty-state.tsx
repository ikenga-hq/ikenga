import { Button } from '@/components/ui/button';
import { cn } from '@/components/ui/utils';
import type { BaseStateProps, StateAction } from './types';

// D-07 `states` contact sheet — the empty variant. "An empty state that
// explains itself and then offers nothing is a dead end; one that offers
// three is a menu." `action` is required (not optional) here specifically:
// an EmptyState with zero actions is the one shape the design spec forbids.
//
// designs/system-flows.html?state=states — cells tagged "empty".

export interface EmptyStateProps extends BaseStateProps {
	action: StateAction;
}

export function EmptyState({
	icon: Icon,
	heading,
	body,
	action,
	fill,
	className,
	...rest
}: EmptyStateProps) {
	return (
		<div
			{...rest}
			role="status"
			className={cn(
				'flex w-full flex-col items-center justify-center gap-2 p-6 text-center',
				fill ? 'h-full' : 'min-h-[168px]',
				className
			)}
		>
			{Icon && (
				<Icon aria-hidden="true" className="size-7" style={{ color: 'var(--fg-faint)' }} />
			)}
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
			<Button
				type="button"
				size="sm"
				variant={action.variant === 'destructive' ? 'destructive' : 'outline'}
				onClick={action.onClick}
				className="mt-1 min-h-8"
			>
				{action.label}
			</Button>
		</div>
	);
}
