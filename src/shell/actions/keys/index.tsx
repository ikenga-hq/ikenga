// Wave 12e placeholder (item 19): WP-60 replaces this file with the real
// Keys surface. Renders a pending `EmptyState` so `/settings/actions/keys`
// is a real, navigable route today. `ActionsSurfaceProps` (`../types`) is the
// mount contract every 12e WP binds to. WP-60 reuses
// `../shared/key-recorder.tsx` for its rebind UI.

import { Keyboard } from 'lucide-react';
import { EmptyState } from '@/components/states';
import type { ActionsSurfaceProps } from '../types';

export function KeysSurface({ onNavigate }: ActionsSurfaceProps) {
	return (
		<EmptyState
			data-state="keys"
			fill
			icon={Keyboard}
			heading="The Keys tab is on its way"
			body="Every keybinding, its conflicts, and rebinding by pressing keys lands here in a follow-up work package (WP-60)."
			action={{ label: 'Back to Actions', onClick: () => onNavigate('actions') }}
		/>
	);
}
