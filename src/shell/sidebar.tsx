import { useShellStore } from '@/lib/shell/shell-store';
import { Explorer } from './explorer/explorer';

/** D-01: the section each rail mode's sidebar is pinned to. Project shows the
 *  whole Explorer (no entry). */
const MODE_SECTION = {
	chi: 'sessions',
	ngwa: 'ngwa-project',
} as const;

export function Sidebar() {
	const activeMode = useShellStore((s) => s.activeMode);
	const only = activeMode === 'chi' || activeMode === 'ngwa' ? MODE_SECTION[activeMode] : undefined;

	// Settings has no sidebar of its own: D-03 puts the section nav inside the
	// pane (`shell/settings/nav.tsx`), so the sidebar stays the Explorer rather
	// than listing the same nine sections a second time.
	return (
		<nav
			aria-label="Explorer sidebar"
			className="flex h-full flex-col border-r border-border bg-card"
		>
			<Explorer only={only} />
		</nav>
	);
}
