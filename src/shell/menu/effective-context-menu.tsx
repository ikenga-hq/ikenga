// WP-55 — one `ContextMenu` rendered from the effective model (G-ACTIONS
// §1.3/§1.4). Every row / tab / section-header context menu goes through
// `EffectiveContextMenu`: the Explorer rows (Files, Artifacts, Sessions,
// Automations, Ngwa (project), Scratchpads, Todos, Views), section headers,
// pane tabs, the address-bar merged row and the rail's section and Ngwa-key
// menus.
//
// Nothing resolves while the menu is closed: the menu body (`useEffectiveMenu`
// subscription + `resolveMenuItems`) is a child component mounted only while
// this menu is open, so a Files tree with hundreds of rows holds no store
// subscription and does no resolution per row until one row's menu opens.

import { type ReactNode, useState } from 'react';
import {
	ContextMenu,
	ContextMenuContent,
	ContextMenuItem,
	ContextMenuSeparator,
	ContextMenuShortcut,
	ContextMenuTrigger,
} from '@/components/ui/context-menu';
import { useEffectiveMenu } from '@/lib/actions/store';
import { type ResolvedMenuRow, type ResolveMenuOptions, resolveMenuItems } from './resolve';

export interface EffectiveContextMenuProps extends ResolveMenuOptions {
	menuId: string;
	/** The trigger (rendered `asChild`). */
	children: ReactNode;
	contentClassName?: string;
	/** Disables the trigger: right-click falls through to the browser default. */
	triggerDisabled?: boolean;
	/** Rendered above the rows (a non-interactive caption). */
	header?: ReactNode;
}

/** The rows of a resolved menu as `ContextMenuItem`s (plain items only: a
 *  checkbox / radio / submenu row renders as a plain item here). */
export function ContextMenuRows({ rows }: { rows: readonly ResolvedMenuRow[] }) {
	return (
		<>
			{rows.map((row, i) =>
				row.kind === 'separator' ? (
					// biome-ignore lint/suspicious/noArrayIndexKey: separators are unkeyed structural markers
					<ContextMenuSeparator key={`sep-${i}`} />
				) : (
					<ContextMenuItem
						key={row.id}
						data-action={row.dataAction}
						disabled={row.disabled}
						title={row.disabledReason}
						variant={row.danger ? 'destructive' : undefined}
						onSelect={row.run}
					>
						{row.icon}
						{row.label}
						{row.shortcut && <ContextMenuShortcut>{row.shortcut}</ContextMenuShortcut>}
					</ContextMenuItem>
				)
			)}
		</>
	);
}

function EffectiveContextMenuBody({
	menuId,
	contentClassName,
	header,
	opts,
}: {
	menuId: string;
	contentClassName?: string;
	header?: ReactNode;
	opts: ResolveMenuOptions;
}) {
	const menu = useEffectiveMenu(menuId);
	const rows = resolveMenuItems(menu, opts);
	if (rows.length === 0) return null;
	return (
		<ContextMenuContent className={contentClassName}>
			{header}
			<ContextMenuRows rows={rows} />
		</ContextMenuContent>
	);
}

export function EffectiveContextMenu({
	menuId,
	children,
	contentClassName,
	triggerDisabled,
	header,
	...opts
}: EffectiveContextMenuProps) {
	const [open, setOpen] = useState(false);
	return (
		<ContextMenu onOpenChange={setOpen}>
			<ContextMenuTrigger asChild disabled={triggerDisabled}>
				{children}
			</ContextMenuTrigger>
			{open && !triggerDisabled && (
				<EffectiveContextMenuBody menuId={menuId} contentClassName={contentClassName} header={header} opts={opts} />
			)}
		</ContextMenu>
	);
}
