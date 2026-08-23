import {
	Bot,
	DatabaseZap,
	FolderKanban,
	Grid3x3,
	HardDrive,
	Info,
	KeyRound,
	LayoutGrid,
	Package,
	Palette,
	Plug,
	ShieldAlert,
	Sparkles,
	Stethoscope,
	Terminal,
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

const NAV: NavSection[] = [
	{
		label: 'Workspace',
		items: [
			{ to: '/settings/appearance', label: 'Appearance', Icon: Palette },
			{ to: '/settings/projects', label: 'Projects', Icon: FolderKanban },
			{ to: '/settings/activity-bar', label: 'Activity bar', Icon: LayoutGrid },
			{ to: '/settings/artifact-grid', label: 'Artifact grid', Icon: Grid3x3 },
			{ to: '/settings/agent', label: 'Agent', Icon: Bot },
			{ to: '/settings/terminal', label: 'Terminal', Icon: Terminal },
			{ to: '/settings/packages', label: 'Packages', Icon: Package },
			{ to: '/settings/pkg-audit', label: 'Pkg violations', Icon: ShieldAlert },
			{ to: '/settings/pkg-health', label: 'Pkg health', Icon: Stethoscope },
			{ to: '/settings/data-health', label: 'Data health', Icon: DatabaseZap },
			{ to: '/settings/onboarding', label: 'Onboarding', Icon: Sparkles },
		],
	},
	{
		label: 'Integrations',
		items: [
			{ to: '/settings/integrations', label: 'Integrations', Icon: Plug },
			{ to: '/settings/secrets', label: 'Secrets', Icon: KeyRound },
		],
	},
	{
		label: 'Storage',
		items: [{ to: '/settings/storage', label: 'Storage', Icon: HardDrive }],
	},
	{
		label: 'Other',
		items: [{ to: '/settings/about', label: 'About', Icon: Info }],
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
