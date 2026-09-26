import type React from 'react';
import type { ReactNode } from 'react';
import { ChevronDown, ChevronRight } from 'lucide-react';
import {
	ContextMenu,
	ContextMenuContent,
	ContextMenuItem,
	ContextMenuSeparator,
	ContextMenuTrigger,
} from '@/components/ui/context-menu';
import { useEffectiveMenu } from '@/lib/actions/store';
import { resolveMenuItems } from '@/shell/menu/resolve';
import type { ExplorerSectionDefinition, ExplorerSectionContext } from './section-registry';

interface SectionFrameProps {
	section: ExplorerSectionDefinition;
	context: ExplorerSectionContext;
	isOpen: boolean;
	onToggle: (exclusive: boolean) => void;
	onCollapseOthers?: () => void;
	onHideSection?: () => void;
	canMoveUp?: boolean;
	canMoveDown?: boolean;
	onMoveUp?: () => void;
	onMoveDown?: () => void;
	children: ReactNode;
}

export function SectionFrame({
	section,
	context,
	isOpen,
	onToggle,
	onCollapseOthers,
	onHideSection,
	canMoveUp = true,
	canMoveDown = true,
	onMoveUp,
	onMoveDown,
	children,
}: SectionFrameProps) {
	const Icon = typeof section.icon === 'string' ? null : section.icon;

	const hookCount = section.useCount?.(context);
	const fnCount = section.count?.(context);
	const rawCount = hookCount ?? fnCount;
	const count = typeof rawCount === 'number' && rawCount > 0 ? rawCount : undefined;
	const badge = section.badge?.(context);
	const hasBadgeOrCount = (badge && badge !== '0') || (count !== undefined && count > 0);
	const displayCount = count !== undefined ? (count > 99 ? '99+' : String(count)) : badge;

	const handleToggle = (e: React.MouseEvent) => {
		e.preventDefault();
		onToggle(e.altKey);
	};

	const ariaLabel = `${section.title}${count !== undefined ? `, ${count} items` : ''}`;

	const sectionMenu = useEffectiveMenu(`section/${section.id}`);
	const menuRows = resolveMenuItems(sectionMenu, {
		handlers: {
			'section.collapse-others': () => onCollapseOthers?.(),
			'section.hide': () => onHideSection?.(),
			'section.move-up': () => onMoveUp?.(),
			'section.move-down': () => onMoveDown?.(),
		},
		disabled: (id) =>
			id === 'section.move-up' ? !canMoveUp : id === 'section.move-down' ? !canMoveDown : false,
	});

	return (
		<div className="flex flex-col border-b border-border last:border-b-0" data-explorer-section={section.id}>
			<ContextMenu>
				<ContextMenuTrigger asChild>
					<button
						type="button"
						data-explorer-row="header"
						data-explorer-header="true"
						data-section-id={section.id}
						className="flex items-center gap-1.5 px-2 py-1.5 hover:bg-accent/50 focus-visible:bg-accent focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring focus:outline-none w-full text-left transition-colors min-h-[28px]"
						aria-expanded={isOpen}
						aria-label={ariaLabel}
						onClick={handleToggle}
					>
						{isOpen ? (
							<ChevronDown className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
						) : (
							<ChevronRight className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
						)}
						{Icon && <Icon className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />}
						<span className="flex-1 truncate text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
							{section.title}
						</span>
						{hasBadgeOrCount && displayCount && (
							<span
								className="shrink-0 bg-primary/10 text-primary px-1.5 rounded-full text-[10px] font-medium"
								aria-hidden="true"
							>
								{displayCount}
							</span>
						)}
					</button>
				</ContextMenuTrigger>
				{menuRows.length > 0 && (
					<ContextMenuContent>
						{menuRows.map((row, i) =>
							row.kind === 'separator' ? (
								// biome-ignore lint/suspicious/noArrayIndexKey: separators are unkeyed structural markers
								<ContextMenuSeparator key={`sep-${i}`} />
							) : (
								<ContextMenuItem key={row.id} disabled={row.disabled} onSelect={row.run}>
									{row.label}
								</ContextMenuItem>
							)
						)}
					</ContextMenuContent>
				)}
			</ContextMenu>
			{isOpen && (
				<div className="flex-1 min-h-0 bg-background/50">
					{children}
				</div>
			)}
		</div>
	);
}
