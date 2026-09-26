import type React from 'react';
import type { ReactNode } from 'react';
import { ArrowDown, ArrowUp, ChevronDown, ChevronRight, EyeOff, FoldVertical } from 'lucide-react';
import { EffectiveContextMenu } from '@/shell/menu/effective-context-menu';
import type { ExplorerSectionDefinition, ExplorerSectionContext } from './section-registry';

// Shipped presentation of the section-header menu (the registry names read
// "Move section up/down" for the Actions tab; the menu keeps its wording).
const SECTION_MENU_LABELS: Readonly<Record<string, string>> = {
	'section.move-up': 'Move up',
	'section.move-down': 'Move down',
};
const SECTION_MENU_ICONS = {
	'section.collapse-others': <FoldVertical className="h-3.5 w-3.5 mr-2" />,
	'section.hide': <EyeOff className="h-3.5 w-3.5 mr-2" />,
	'section.move-up': <ArrowUp className="h-3.5 w-3.5 mr-2" />,
	'section.move-down': <ArrowDown className="h-3.5 w-3.5 mr-2" />,
};

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

	// `section/<id>` (G-ACTIONS §1.3), resolved only while the menu is open.
	const menuHandlers = {
		'section.collapse-others': () => onCollapseOthers?.(),
		'section.hide': () => onHideSection?.(),
		'section.move-up': () => onMoveUp?.(),
		'section.move-down': () => onMoveDown?.(),
	};
	const menuDisabled = (id: string) =>
		id === 'section.move-up' ? !canMoveUp : id === 'section.move-down' ? !canMoveDown : false;

	return (
		<div className="flex flex-col border-b border-border last:border-b-0" data-explorer-section={section.id}>
			<EffectiveContextMenu
				menuId={`section/${section.id}`}
				handlers={menuHandlers}
				disabled={menuDisabled}
				labels={SECTION_MENU_LABELS}
				icons={SECTION_MENU_ICONS}
			>
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
			</EffectiveContextMenu>
			{isOpen && (
				<div className="flex-1 min-h-0 bg-background/50">
					{children}
				</div>
			)}
		</div>
	);
}
