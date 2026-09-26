import { useShellStore } from '@/lib/shell/shell-store';
import { Explorer } from './explorer/explorer';

export function Sidebar() {
	const activeMode = useShellStore((s) => s.activeMode);

	if (activeMode === 'chi' || activeMode === 'ngwa') {
		return null;
	}

	// Settings has no sidebar of its own: D-03 puts the section nav inside the
	// pane (`shell/settings/nav.tsx`), so the sidebar stays the Explorer rather
	// than listing the same nine sections a second time.
	return (
		<nav
			aria-label="Explorer sidebar"
			className="flex h-full flex-col border-r border-border bg-card"
		>
			<Explorer />
		</nav>
	);
}
