import { AlertCircle } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { cn } from '@/components/ui/utils';
import type { BaseStateProps, StateAction } from './types';

// D-07 `states` contact sheet — the error variant (pane crashed / spawn
// failed / kernel error, etc.). `action` stays optional and singular: some
// errors are informational only, but a second recovery action is never
// offered alongside a retry.
//
// designs/system-flows.html?state=states — cells tagged with the red/`err` tone.

export interface ErrorStateProps extends BaseStateProps {
	action?: StateAction;
}

export function ErrorState({
	icon: Icon = AlertCircle,
	heading,
	body,
	action,
	fill,
	className,
	...rest
}: ErrorStateProps) {
	return (
		<div
			{...rest}
			role="alert"
			aria-live="assertive"
			className={cn(
				'flex w-full flex-col items-center justify-center gap-2 rounded-md border p-6 text-center',
				fill ? 'h-full' : 'min-h-[168px]',
				className
			)}
			style={{
				borderColor: 'color-mix(in srgb, var(--danger) 50%, transparent)',
				background: 'var(--danger-soft)',
			}}
		>
			<Icon aria-hidden="true" className="size-7" style={{ color: 'var(--danger)' }} />
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
			{action && (
				<Button
					type="button"
					size="sm"
					variant={action.variant === 'destructive' ? 'destructive' : 'outline'}
					onClick={action.onClick}
					className="mt-1 min-h-8"
				>
					{action.label}
				</Button>
			)}
		</div>
	);
}
