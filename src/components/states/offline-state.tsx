import { CloudOff } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { cn } from '@/components/ui/utils';
import type { BaseStateProps, StateAction } from './types';

// D-07 `states` contact sheet — the offline variant (registry unreachable,
// sidecar down, etc.). "Everything installed still runs. Only browsing and
// installing new packages needs the network." — offline is a warn tone, not
// an error tone: `--achievement` (amber), not `--danger`.
//
// designs/system-flows.html?state=states — the "offline" / `warnc` cells.

export interface OfflineStateProps extends BaseStateProps {
	action?: StateAction;
}

export function OfflineState({
	icon: Icon = CloudOff,
	heading,
	body,
	action,
	fill,
	className,
	...rest
}: OfflineStateProps) {
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
			<Icon aria-hidden="true" className="size-7" style={{ color: 'var(--achievement)' }} />
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
					variant="outline"
					onClick={action.onClick}
					className="mt-1 min-h-8"
				>
					{action.label}
				</Button>
			)}
		</div>
	);
}
