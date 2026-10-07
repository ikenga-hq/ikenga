// D-06 shared part (WP-57): the per-row/detail icon glyph — reuses the same
// name-resolved lucide icon + kebab-case-normalize pattern as `shell/pin-icon.tsx`
// (`action.icon` is a free-form lucide name a user or a pkg manifest wrote;
// `UserAction.icon`'s own doc default is `zap`).

import { type LucideIcon, Zap } from 'lucide-react';
import { NamedLucideIcon } from '@/lib/icons/named-lucide-icon';
import { normalizeLucideName } from '@/shell/pin-icon';

export interface ActionIconProps {
	icon?: string | null;
	className?: string;
	Fallback?: LucideIcon;
}

export function ActionIcon({ icon, className = 'h-3.5 w-3.5', Fallback = Zap }: ActionIconProps) {
	const lucide = normalizeLucideName(icon);
	if (!lucide) return <Fallback className={className} aria-hidden="true" />;
	return (
		<NamedLucideIcon name={lucide} Fallback={Fallback} className={className} aria-hidden="true" />
	);
}
