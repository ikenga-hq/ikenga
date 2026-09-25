import {
	Bot,
	FolderKanban,
	HardDrive,
	Info,
	KeyRound,
	Palette,
	Plug,
	SlidersHorizontal,
	Users,
	type LucideIcon,
} from 'lucide-react';

import { usePaneStore } from '@/lib/panes/pane-store';
import { findLeaf } from '@/lib/panes/pane-reducer';
import { SidebarNav, SidebarNavRow, SidebarNavSection } from './_nav';

interface NavItem {
	to: string;
	label: string;
	Icon: LucideIcon;
}

interface NavSection {
	label: string;
	items: NavItem[];
}

// WP-35: mirrors the D-03 nine-section settings shell. The retired per-page
// routes (activity-bar, agent, terminal, artifact-grid, backup, onboarding,
// packages, pkg-audit, pkg-health, data-health) redirect from the router.
const NAV: NavSection[] = [
	{
		label: 'Workspace',
		items: [
			{ to: '/settings/appearance', label: 'Appearance', Icon: Palette },
			{ to: '/settings/projects', label: 'Projects', Icon: FolderKanban },
			{ to: '/settings/engines', label: 'Chi & engines', Icon: Bot },
			{ to: '/settings/workspace', label: 'Workspace', Icon: SlidersHorizontal },
		],
	},
	{
		label: 'Access',
		items: [
			{ to: '/settings/secrets', label: 'Secrets', Icon: KeyRound },
			{ to: '/settings/integrations', label: 'Integrations', Icon: Plug },
			{ to: '/settings/people', label: 'People & devices', Icon: Users },
		],
	},
	{
		label: 'System',
		items: [
			{ to: '/settings/storage', label: 'Storage & backup', Icon: HardDrive },
			{ to: '/settings/about', label: 'Updates & about', Icon: Info },
		],
	},
];

export function SettingsMode() {
	const navigateFocused = usePaneStore((s) => s.navigateFocused);
	const activePath = usePaneStore((s) => {
		const leaf = findLeaf(s.root, s.focusedId);
		if (!leaf) return null;
		const tab = leaf.tabs[leaf.activeTabIdx];
		return tab && tab.kind === 'route' ? tab.path : null;
	});

	return (
		<SidebarNav ariaLabel="Settings navigation">
			{NAV.map((sec) => (
				<SidebarNavSection key={sec.label} label={sec.label}>
					{sec.items.map(({ to, label, Icon }) => {
						const isActive = activePath === to || activePath?.startsWith(`${to}/`) === true;
						return (
							<SidebarNavRow
								key={to}
								icon={Icon}
								label={label}
								active={isActive}
								onSelect={() => navigateFocused(to)}
							/>
						);
					})}
				</SidebarNavSection>
			))}
		</SidebarNav>
	);
}
