// Default sidebar entries for the interim App menu, which the sidebar shows
// for the Project and Chi rail modes until WP-04's Explorer replaces it.
// These are shell-internal surfaces only — app pkgs contribute their own nav
// via the UiRoutesRegistry and declarative `ui.views` blocks in their
// manifests, surfaced by the kernel snapshot. The pkg-aware list is rendered
// alongside this one inside AppMode, and since WP-03 a package's rail
// presence is a pin (seeded once from `ui.views[0]` by WP-22), never a rail
// mode of its own.
//
// Packages-related nav (catalog, updates, trust, store) belongs to Ngwa
// (rail ⌘3; its key's context menu opens Installed / Store / Health); it
// isn't a concern of this menu.

import { Activity, CheckSquare, Clock, FileText, Home } from 'lucide-react';

export interface NavItem {
	to: string;
	label: string;
	Icon: typeof Home;
}
export interface NavGroup {
	label: string | null;
	items: NavItem[];
}

export const NAV_GROUPS: NavGroup[] = [
	{
		label: null,
		items: [
			{ to: '/', label: 'Home', Icon: Home },
			// `/claude` lives under the Ngwa rail mode (⌘3). See
			// `src/shell/sidebar-modes/ngwa-mode.tsx`.
		],
	},
	{
		label: 'Project',
		items: [
			{ to: '/scratchpads', label: 'Scratchpads', Icon: FileText },
			{ to: '/todos', label: 'Todos', Icon: CheckSquare },
		],
	},
	{
		// Agent-ops deep-links. These routes auto-redirect to the
		// com.ikenga.agent-ops pkg once it is installed; they show a landing
		// page otherwise. WP-08/WP-12 will wire the live per-view sub-paths.
		label: 'Agents',
		items: [
			{ to: '/cron', label: 'Cron', Icon: Clock },
			{ to: '/agent-runs', label: 'Agent Runs', Icon: Activity },
		],
	},
];
