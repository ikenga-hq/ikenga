import type { LucideIcon } from 'lucide-react';

// WP-43 (D-07 `states` contact sheet, plans/shell-ux-rearchitecture/drafts/design-spec-D-03-07.md
// §D-07): "Each empty state offers exactly one next action." `StateAction` is
// singular by construction — there is no `actions[]` variant anywhere in this
// module, so a second primary action is a type error, not a review comment.

export interface StateAction {
	label: string;
	onClick: () => void;
	/** Rare: a destructive-toned action (e.g. a danger-zone confirmation). */
	variant?: 'default' | 'destructive';
}

export interface BaseStateProps {
	icon?: LucideIcon;
	heading: string;
	body?: React.ReactNode;
	/** Fill the parent (h-full) instead of a natural-height centered box. */
	fill?: boolean;
	className?: string;
	/** Which D-07 state this render is — every shipped state root carries
	 *  this as `data-state` so the conformance pass can grep for it (G-55). */
	'data-state': string;
}
