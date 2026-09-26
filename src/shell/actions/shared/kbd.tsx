// D-06 shared part (WP-57): the keybinding renderer every tab reuses —
// Actions' Key column, the Editor's key field, the Keys tab's Key column and
// the Menus tab's item accelerators. One formatting engine
// (`formatKeyLabel`, G-ACTIONS §3.1) so mac glyphs (⌘⇧E) and the other
// platforms' spelled-out form (Ctrl+Shift+E) never drift from each other.

import { formatKeyLabel, isMacPlatform } from '@/lib/keymap/platform';
import { cn } from '@/components/ui/utils';

export interface KbdProps {
	/** A canonical key sequence (`mod+shift+e`, or a chord `mod+k mod+r`); a
	 *  falsy value renders the `empty` fallback instead. */
	combo?: string | null;
	/** Overrides the live platform — the Keys tab's mac/Windows preview toggle. */
	mac?: boolean;
	/** Rendered in place of a kbd tag when `combo` is empty. Default `'—'`. */
	empty?: string;
	className?: string;
}

/** One key sequence as `<kbd>` chip(s) — a chord renders as two chips. */
export function Kbd({ combo, mac, empty = '—', className }: KbdProps) {
	if (!combo) {
		return <span className={cn('meta', className)}>{empty}</span>;
	}
	const label = formatKeyLabel(combo, { mac: mac ?? isMacPlatform() });
	const strokes = label.split(' ');
	return (
		<span className={cn('kbdrow', className)}>
			{strokes.map((stroke, i) => (
				// biome-ignore lint/suspicious/noArrayIndexKey: strokes never reorder within one combo
				<kbd key={i} className="kbd">
					{stroke}
				</kbd>
			))}
		</span>
	);
}
