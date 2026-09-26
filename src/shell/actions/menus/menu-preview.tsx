// D-06 Menus tab (WP-59): "the real menu, as it will open" — the right-hand
// live preview (`designs/actions.html`'s `renderPreview`). Visible rows
// only, in tree order; a personal/project action keeps the mockup's
// highlighted "yours" treatment (`.isnew`) so a customization stands out
// from the built-in/package baseline even with no selection state involved.

import { formatKeyLabel, isMacPlatform } from '@/lib/keymap/platform';
import { ActionIcon } from '../shared/action-icon';
import type { MenuRow } from './menu-model';

export interface MenuPreviewProps {
	rows: readonly MenuRow[];
	keyById: ReadonlyMap<string, string | null>;
}

export function MenuPreview({ rows, keyById }: MenuPreviewProps) {
	const visible = rows.filter((row) => (row.kind === 'action' ? !row.hidden : true));
	const mac = isMacPlatform();

	return (
		<div className="menupreview">
			<div className="menu inline" role="presentation">
				{visible.map((row, i) => {
					if (row.kind === 'separator') {
						// biome-ignore lint/suspicious/noArrayIndexKey: separators carry no identity beyond position
						return <div key={`sep-${i}`} className="msep" />;
					}
					const { action } = row;
					const combo = keyById.get(row.id);
					const classes = [
						'mitem',
						action.danger ? 'danger' : '',
						action.source === 'personal' || action.source === 'project' ? 'isnew' : '',
					]
						.filter(Boolean)
						.join(' ');
					return (
						<div key={row.id} className={classes}>
							<ActionIcon icon={action.icon} className="h-3.5 w-3.5 shrink-0" />
							<span>{action.name}</span>
							{combo && <span className="k">{formatKeyLabel(combo, { mac })}</span>}
						</div>
					);
				})}
			</div>
		</div>
	);
}

/** The preview column's own count line ("N shown" / "N shown · M hidden"). */
export function previewNote(rows: readonly MenuRow[]): string {
	const shown = rows.filter((row) => row.kind === 'action' && !row.hidden).length;
	const hidden = rows.filter((row) => row.kind === 'action' && row.hidden).length;
	return hidden > 0 ? `${shown} shown · ${hidden} hidden` : `${shown} shown`;
}
