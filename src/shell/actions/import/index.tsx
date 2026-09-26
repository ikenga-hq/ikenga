// Wave 12e placeholder (item 19): WP-61 replaces this file with the real
// Import surface. Renders a pending `EmptyState` so `/settings/actions/import`
// is a real, navigable route today, even though it is not one of the 1–4
// tabs (reached from the header's `⋯` "Import actions…" instead).
// `ActionsSurfaceProps` (`../types`) is the mount contract every 12e WP binds
// to.

import { Download } from 'lucide-react';
import { EmptyState } from '@/components/states';
import type { ActionsSurfaceProps } from '../types';

export function ImportSurface({ onNavigate }: ActionsSurfaceProps) {
	return (
		<EmptyState
			data-state="import"
			fill
			icon={Download}
			heading="Importing actions is on its way"
			body="Bringing in VS Code keybindings (and other sources) with a reviewed diff lands here in a follow-up work package (WP-61)."
			action={{ label: 'Back to Actions', onClick: () => onNavigate('actions') }}
		/>
	);
}
