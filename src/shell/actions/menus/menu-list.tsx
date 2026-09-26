// D-06 Menus tab (WP-59): the left "which menu" column — every menu id the
// effective model materializes (G-ACTIONS §1.3's fixed ids plus any
// `section/<id>` a placement or override references), not a hardcoded
// illustration list, so a package or user action placed in a menu D-06 never
// drew (e.g. a new Explorer section) is still reachable here. `menuLabel`
// (WP-57 shared part) gives each id the same short label the Actions tab's
// Placement facet and detail pane already use.

import { ListTree } from 'lucide-react';
import type { EffectiveModel } from '@/lib/actions/store';
import { menuLabel } from '../shared/menu-label';

export interface MenuListProps {
	menuIds: readonly string[];
	model: EffectiveModel;
	selected: string | null;
	onSelect: (menuId: string) => void;
}

/** Rows configured for this menu: visible items plus hidden ones (the tree's
 *  own count, `buildMenuRows`' shape) — not just what would currently pop
 *  open, so a menu that is entirely hidden still reads as non-empty here. */
function rowCount(model: EffectiveModel, menuId: string): number {
	const menu = model.menus.get(menuId);
	if (!menu) return 0;
	return menu.items.filter((item) => item.kind === 'action').length + menu.hidden.length;
}

export function MenuList({ menuIds, model, selected, onSelect }: MenuListProps) {
	return (
		<div className="menulist" role="tablist" aria-label="Menus">
			{menuIds.map((id) => {
				const on = id === selected;
				return (
					<button
						key={id}
						type="button"
						role="tab"
						aria-selected={on}
						data-m={id}
						className={`mrow${on ? ' on' : ''}`}
						onClick={() => onSelect(id)}
					>
						<ListTree className="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
						<span>{menuLabel(id)}</span>
						<span className="n">{rowCount(model, id)}</span>
					</button>
				);
			})}
		</div>
	);
}
