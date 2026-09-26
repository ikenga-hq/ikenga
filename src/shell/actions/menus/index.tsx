// Wave 12e placeholder (item 19): WP-59 replaces this file with the real
// Menus surface. Renders a pending `EmptyState` so `/settings/actions/menus`
// is a real, navigable route today. `ActionsSurfaceProps` (`../types`) is the
// mount contract every 12e WP binds to.

import { ListTree } from 'lucide-react';
import { EmptyState } from '@/components/states';
import type { ActionsSurfaceProps } from '../types';

export function MenusSurface({ onNavigate }: ActionsSurfaceProps) {
	return (
		<EmptyState
			data-state="menus"
			fill
			icon={ListTree}
			heading="The Menus tab is on its way"
			body="Reordering, hiding and adding actions to a menu, with a live preview, lands here in a follow-up work package (WP-59)."
			action={{ label: 'Back to Actions', onClick: () => onNavigate('actions') }}
		/>
	);
}
