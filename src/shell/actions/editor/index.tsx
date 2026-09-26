// Wave 12e placeholder (item 19): WP-58 replaces this file with the real
// Editor surface. Renders a pending `EmptyState` so `/settings/actions/editor`
// is a real, navigable route today. `ActionsSurfaceProps` (`../types`) is the
// mount contract every 12e WP binds to.

import { FileCode } from 'lucide-react';
import { EmptyState } from '@/components/states';
import type { ActionsSurfaceProps } from '../types';

export function EditorSurface({ onNavigate }: ActionsSurfaceProps) {
	return (
		<EmptyState
			data-state="editor"
			fill
			icon={FileCode}
			heading="The Editor tab is on its way"
			body="Writing a new action, or editing one of your own, lands here in a follow-up work package (WP-58)."
			action={{ label: 'Back to Actions', onClick: () => onNavigate('actions') }}
		/>
	);
}
