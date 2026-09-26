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
	/** Text of the dashed "unbound" kbd shown when `combo` is empty (D-06
	 *  `kbd(a.key)`: `<kbd class="key none">unbound</kbd>`). Default `'unbound'`. */
	empty?: string;
	className?: string;
}

/** One key sequence as `<kbd>` chip(s) — a chord renders as two chips. An
 *  unset key still renders as a `<kbd>` (dashed, muted), not plain text, so
 *  the Key column keeps one visual shape whether bound or not. */
export function Kbd({ combo, mac, empty = 'unbound', className }: KbdProps) {
	if (!combo) {
		return (
			<kbd className={cn('kbd kbd-none', className)}>{empty}</kbd>
		);
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
