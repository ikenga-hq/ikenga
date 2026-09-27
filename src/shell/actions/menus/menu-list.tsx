// D-06 Menus tab (WP-59): the left "which menu" column.
//
// Review round 1 minor: D-06's own `PLACES` fixture (`designs/actions.html`
// — palette, files, artifacts, session, tab, pane, section, status, rail,
// native) is exactly ten entries, matching the brief's "Files, Artifacts,
// Session, Tab, Pane ⋯, Explorer section ⋯, Status bar, Rail, Palette
// groups, Native menu". `model.menus.ids` (every known menu id — the fixed
// ones plus any `section/<id>` / `native/<top>` a placement or override
// references) drove a flat one-row-per-id list before this pass; that isn't
// D-06's list, and it means the two parameterized families showed up as one
// row apiece instead of grouped. Two of the ten ("Explorer section ⋯",
// "Native menu") are groups: one row selects the category, a picker beneath
// it chooses the concrete id — never listed flat, and left out entirely
// when the model has none (an empty group is worse than no row).

import { ListTree } from 'lucide-react';
import type { EffectiveModel } from '@/lib/actions/store';
import { menuLabel } from '../shared/menu-label';

export interface MenuListProps {
	/** Every menu id the model materializes — used only to find each group's
	 *  concrete members (`section/*`, `native/*`); the fixed places below are
	 *  otherwise not derived from it. */
	menuIds: readonly string[];
	model: EffectiveModel;
	selected: string | null;
	onSelect: (menuId: string) => void;
}

interface Place {
	readonly id: string;
	readonly label: string;
	/** A direct menu id (most places), or omitted for a group (below). */
	readonly menuId?: string;
	/** A group's id prefix (`section/`, `native/`) — its members come from
	 *  `menuIds` at render time, D-06's fixture has none of these for real. */
	readonly prefix?: string;
}

/** D-06's exact ten (`designs/actions.html`'s `PLACES`), in its order. */
const PLACES: readonly Place[] = [
	{ id: 'files', label: 'Files', menuId: 'files' },
	{ id: 'artifacts', label: 'Artifacts', menuId: 'artifacts' },
	{ id: 'session', label: 'Session', menuId: 'session' },
	{ id: 'tab', label: 'Tab', menuId: 'tab' },
	{ id: 'pane', label: 'Pane ⋯', menuId: 'pane' },
	{ id: 'section', label: 'Explorer section ⋯', prefix: 'section/' },
	{ id: 'status', label: 'Status bar', menuId: 'status' },
	{ id: 'rail', label: 'Rail', menuId: 'rail' },
	{ id: 'palette', label: 'Palette groups', menuId: 'palette' },
	{ id: 'native', label: 'Native menu', prefix: 'native/' },
];

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
			{PLACES.map((place) => {
				if (place.menuId) {
					const targetId = place.menuId;
					const on = selected === targetId;
					return (
						<button
							key={place.id}
							type="button"
							role="tab"
							aria-selected={on}
							data-m={targetId}
							className={`mrow${on ? ' on' : ''}`}
							onClick={() => onSelect(targetId)}
						>
							<ListTree className="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
							<span>{place.label}</span>
							<span className="n">{rowCount(model, targetId)}</span>
						</button>
					);
				}

				const prefix = place.prefix as string;
				const members = menuIds.filter((id) => id.startsWith(prefix)).sort();
				if (members.length === 0) return null;
				const activeMember = selected && members.includes(selected) ? selected : members[0];
				const on = selected != null && members.includes(selected);

				return (
					<div key={place.id} className="mrow-group">
						<button
							type="button"
							role="tab"
							aria-selected={on}
							data-m={place.id}
							className={`mrow${on ? ' on' : ''}`}
							onClick={() => onSelect(activeMember)}
						>
							<ListTree className="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
							<span>{place.label}</span>
							<span className="n">{rowCount(model, activeMember)}</span>
						</button>
						<select
							className="mrow-group-pick"
							aria-label={`${place.label} — choose one`}
							value={activeMember}
							onChange={(e) => onSelect(e.target.value)}
						>
							{members.map((id) => (
								<option key={id} value={id}>
									{menuLabel(id)}
								</option>
							))}
						</select>
					</div>
				);
			})}
		</div>
	);
}
