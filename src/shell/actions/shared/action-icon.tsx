// D-06 shared part (WP-57): the per-row/detail icon glyph — reuses the same
// lucide-dynamic-icon + kebab-case-normalize pattern as `shell/pin-icon.tsx`
// (`action.icon` is a free-form lucide name a user or a pkg manifest wrote;
// `UserAction.icon`'s own doc default is `zap`).

import { Zap, type LucideIcon } from 'lucide-react';
import { DynamicIcon, type IconName } from 'lucide-react/dynamic';
import { Suspense } from 'react';
import { normalizeLucideName } from '@/shell/pin-icon';

export interface ActionIconProps {
	icon?: string | null;
	className?: string;
	Fallback?: LucideIcon;
}

export function ActionIcon({ icon, className = 'h-3.5 w-3.5', Fallback = Zap }: ActionIconProps) {
	const lucide = normalizeLucideName(icon) as IconName | null;
	if (!lucide) return <Fallback className={className} aria-hidden="true" />;
	return (
		<Suspense fallback={<Fallback className={className} aria-hidden="true" />}>
			<DynamicIcon name={lucide} className={className} aria-hidden="true" />
		</Suspense>
	);
}
