// D-18: when WSL couldn't be asked, an agent list shows this ONE notice above
// its rows ("WSL unavailable — <reason>", the reason from the first affected
// agent) and each affected row carries only a short dimmed chip
// (`WslUnavailableChip`), instead of repeating the full reason on every row.

import type * as React from 'react';
import { AlertTriangle } from 'lucide-react';

import { Banner } from '@/components/ui/banner';
import { StatusChip } from '@/components/ui/status-chip';
import { cn } from '@/components/ui/utils';

export interface WslUnavailableNoticeProps {
	/** "WSL unavailable — <reason>" (see `firstAgentUnavailableText`). */
	text: string;
	/** Right-aligned actions (e.g. Re-check). */
	actions?: React.ReactNode;
	className?: string;
}

export function WslUnavailableNotice({ text, actions, className }: WslUnavailableNoticeProps) {
	return (
		<Banner
			tone="warning"
			icon={<AlertTriangle />}
			role="alert"
			className={cn('rounded-md border', className)}
			actions={actions}
			data-testid="wsl-unavailable-notice"
		>
			<div className="text-[13px] font-semibold" data-testid="wsl-unavailable-notice-text">
				{text}
			</div>
			<div className="mt-1 text-xs" style={{ color: 'var(--fg-muted)' }}>
				Engines that run inside WSL couldn&apos;t be checked — this isn&apos;t a missing install.
				Check that WSL starts (run <span className="font-mono">wsl.exe</span> in a terminal, or see
				the WSL health check if Ikenga shows one), then re-check.
			</div>
		</Banner>
	);
}

/** The short dimmed per-row marker for an agent covered by the notice. The
 *  full reason stays available as a tooltip. */
export function WslUnavailableChip({ title, testId }: { title?: string; testId?: string }) {
	return (
		<span title={title} data-testid={testId}>
			<StatusChip tone="faint">WSL unavailable</StatusChip>
		</span>
	);
}
