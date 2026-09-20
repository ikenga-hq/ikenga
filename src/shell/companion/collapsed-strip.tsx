// The Companion at rest — spec §5.1, #55–#57. A 36 px strip that is ONE
// button (expand). Top to bottom: expand chevron, attention glyph + count
// (shield while a permission is pending, otherwise the run pulse, otherwise
// nothing), and the vertical label `Chi · <state>`. It accepts tab drops.

import { ChevronLeft, ShieldAlert } from 'lucide-react';
import { labelFor } from '@/lib/keymap/registry';
import type { DropTargetProps } from '@/lib/panes/pointer-drag';
import { cn } from '@/components/ui/utils';

export const COLLAPSED_WIDTH = 36;

export interface CollapsedStripProps {
	pendingPermissions: number;
	/** A session in the Companion is running. */
	live: boolean;
	onExpand: () => void;
	dropProps?: DropTargetProps;
	dropHover?: boolean;
}

/** `Chi · 1 pending` / `Chi · live` / `Chi`. */
export function stripStateLabel(pending: number, live: boolean): string {
	if (pending > 0) return `Chi · ${pending} pending`;
	if (live) return 'Chi · live';
	return 'Chi';
}

/** §5.1 accessible name: "Chi companion, collapsed, 1 permission pending.
 *  Expand (⌘J)." — the key label comes from the keymap registry. */
export function stripAccessibleName(pending: number, live: boolean): string {
	const parts = ['Chi companion', 'collapsed'];
	if (pending > 0) parts.push(`${pending} permission${pending === 1 ? '' : 's'} pending`);
	else if (live) parts.push('a session is live');
	const key = labelFor('companion.toggle');
	return `${parts.join(', ')}. Expand${key ? ` (${key})` : ''}.`;
}

export function CollapsedStrip({
	pendingPermissions,
	live,
	onExpand,
	dropProps,
	dropHover,
}: CollapsedStripProps) {
	return (
		<aside
			aria-label="Chi companion"
			className="flex h-full shrink-0 border-l"
			style={{ width: `${COLLAPSED_WIDTH}px`, borderColor: 'var(--border-soft)' }}
			{...dropProps}
		>
			<button
				type="button"
				aria-expanded={false}
				aria-label={stripAccessibleName(pendingPermissions, live)}
				onClick={onExpand}
				className={cn(
					'flex h-full w-full flex-col items-center gap-3 py-2 text-[var(--fg-muted)] hover:bg-[var(--bg-raised)] hover:text-[var(--fg)]',
					'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring',
					dropHover ? 'bg-[var(--primary-soft)]' : 'bg-[var(--bg-base)]'
				)}
			>
				<span className="grid size-6 place-items-center" aria-hidden="true">
					<ChevronLeft className="h-3.5 w-3.5" />
				</span>
				{pendingPermissions > 0 ? (
					<span
						className="relative grid size-6 place-items-center"
						aria-hidden="true"
						data-attention="permission"
					>
						<ShieldAlert className="h-4 w-4" style={{ color: 'var(--achievement)' }} />
						<span
							className="absolute -right-1 -top-1 min-w-3.5 rounded-full px-0.5 text-center font-mono text-[9px] leading-[14px]"
							style={{ background: 'var(--achievement)', color: 'var(--bg-base)' }}
						>
							{pendingPermissions}
						</span>
					</span>
				) : live ? (
					<span className="grid size-6 place-items-center" aria-hidden="true" data-attention="run">
						<span
							className="size-2 rounded-full motion-safe:animate-pulse"
							style={{ background: 'var(--live)' }}
						/>
					</span>
				) : null}
				<span aria-hidden="true" className="text-[11px] tracking-wider [writing-mode:vertical-rl]">
					{stripStateLabel(pendingPermissions, live)}
				</span>
			</button>
		</aside>
	);
}
