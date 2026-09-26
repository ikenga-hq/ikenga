// WP-58 — a curated set of real Lucide names for the Editor's icon picker.
//
// G-ACTIONS §1.2: `icon` is a free-form Lucide kebab-case name (default
// `zap`); D-06's `ICON_SET` (`i-bolt`, `i-chi`, `i-refresh`, …) is the
// design's own sprite sheet, not Lucide names (§11 item 7 — those ids are
// display-only). This list swaps in real `lucide-react/dynamic` names that
// read the same way in an action picker, plus a free-text fallback input so
// nothing is actually restricted to this set.

export const ACTION_ICON_CHOICES: readonly string[] = [
	'zap',
	'sparkles',
	'send',
	'terminal',
	'file-code',
	'folder',
	'refresh-cw',
	'git-branch',
	'layers',
	'image',
	'sticky-note',
	'search',
	'shield',
	'clock',
	'check',
	'pin',
	'bookmark',
	'copy',
	'play',
	'pause',
	'package',
	'store',
	'database',
	'book-open',
	'grid-2x2',
	'rocket',
	'wrench',
	'bell',
	'flag',
	'link',
	'globe',
	'workflow',
];
