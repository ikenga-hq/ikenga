// WP-55 — one `ContextMenu` rendered from the effective model (G-ACTIONS
// §1.3/§1.4), for the menus whose default contents are a flat list of plain
// items (no checkbox / radio / submenu): the Explorer's stub-array sections
// (Artifacts, Sessions, Automations, Ngwa (project), Scratchpads, Todos,
// Views), the rail's section and Ngwa-key menus. Files, the tab menu, the
// address-bar merged row and the section-header menu keep their own trigger
// markup and call `resolveMenuItems` directly instead of this wrapper.

import type { ReactNode } from 'react';
import {
	ContextMenu,
	ContextMenuContent,
	ContextMenuItem,
	ContextMenuSeparator,
	ContextMenuTrigger,
} from '@/components/ui/context-menu';
import { useEffectiveMenu } from '@/lib/actions/store';
import { resolveMenuItems, type ResolveMenuOptions } from './resolve';

export interface EffectiveContextMenuProps extends ResolveMenuOptions {
	menuId: string;
	children: ReactNode;
	contentClassName?: string;
}

export function EffectiveContextMenu({ menuId, children, contentClassName, ...opts }: EffectiveContextMenuProps) {
	const menu = useEffectiveMenu(menuId);
	const rows = resolveMenuItems(menu, opts);
	return (
		<ContextMenu>
			<ContextMenuTrigger asChild>{children}</ContextMenuTrigger>
			{rows.length > 0 && (
				<ContextMenuContent className={contentClassName}>
					{rows.map((row, i) =>
						row.kind === 'separator' ? (
							// biome-ignore lint/suspicious/noArrayIndexKey: separators are unkeyed structural markers
							<ContextMenuSeparator key={`sep-${i}`} />
						) : (
							<ContextMenuItem
								key={row.id}
								disabled={row.disabled}
								variant={row.danger ? 'destructive' : undefined}
								onSelect={row.run}
							>
								{row.label}
								{row.shortcut && (
									<span className="ml-auto pl-4 text-[10px] text-muted-foreground">{row.shortcut}</span>
								)}
							</ContextMenuItem>
						)
					)}
				</ContextMenuContent>
			)}
		</ContextMenu>
	);
}
