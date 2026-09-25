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

import { Input } from '@/components/ui/input';
import { cn } from '@/components/ui/utils';
import type { SettingsField } from '@/lib/settings/types';

export type SettingsSectionId =
	| 'appearance'
	| 'projects'
	| 'engines'
	| 'workspace'
	| 'secrets'
	| 'integrations'
	| 'people'
	| 'storage'
	| 'about';

export type SettingsScopeId = 'personal' | 'project';

export interface SettingsFieldMeta {
	/** Dotted schema path; `null` for informational rows that hold no value. */
	field: SettingsField | null;
	label: string;
	help?: string;
	keywords?: string;
}

export interface SettingsSectionMeta {
	id: SettingsSectionId;
	label: string;
	Icon: LucideIcon;
	description: string;
	/** Schema fields owned by the section — the reset-section + search surface. */
	fields: SettingsFieldMeta[];
}

export const SETTINGS_SECTIONS: readonly SettingsSectionMeta[] = [
	{
		id: 'appearance',
		label: 'Appearance',
		Icon: Palette,
		description: 'Theme, mode and density are the three knobs that scope every other token.',
		fields: [
			{ field: 'appearance.theme', label: 'Theme', help: 'Dusk Wood, Kola Daylight, Bronze Shrine' },
			{ field: 'appearance.mode', label: 'Mode', help: 'light, dark or system' },
			{ field: 'appearance.density', label: 'Density', help: 'compact, comfortable, spacious' },
			{ field: 'appearance.tintStrength', label: 'Tint strength', help: 'workspace tint' },
		],
	},
	{
		id: 'projects',
		label: 'Projects',
		Icon: FolderKanban,
		description: 'First-class scoping containers for sessions, packages, layout and todos.',
		fields: [{ field: 'projects.extraRoots', label: 'Extra roots', help: 'additional file roots' }],
	},
	{
		id: 'engines',
		label: 'Chi & engines',
		Icon: Bot,
		description: 'Default engine, shells, agent execution target and terminal restore.',
		fields: [
			{ field: 'engines.defaultEngineId', label: 'Default engine', help: 'coding agent' },
			{ field: 'engines.defaultShellId', label: 'Default interactive shell' },
			{ field: 'engines.customShellProfiles', label: 'Custom shell profiles' },
			{ field: 'engines.agentEnvironment', label: 'Agent execution target', help: 'native or wsl' },
			{ field: 'engines.agentWslDistro', label: 'WSL distribution' },
			{ field: 'engines.resumeTerminals', label: 'Resume terminals on start' },
		],
	},
	{
		id: 'workspace',
		label: 'Workspace',
		Icon: SlidersHorizontal,
		description: 'Rail pins, Explorer order, artifact grid behaviour and consecration.',
		fields: [
			{ field: 'workspace.explorerSections', label: 'Explorer sections', help: 'order and visibility' },
			{ field: 'workspace.sidebarCollapsed', label: 'Sidebar collapsed' },
			{ field: 'workspace.artifact.defaultSink', label: 'Artifact default sink' },
			{ field: 'workspace.artifact.stackMode', label: 'Artifact stack mode' },
			{ field: 'workspace.artifact.terminalHandoff', label: 'Artifact terminal handoff' },
			{ field: 'workspace.artifact.folderOverrides', label: 'Folder overrides' },
			{ field: 'workspace.artifact.artifactSinkOverrides', label: 'Artifact sink overrides' },
			{ field: 'workspace.artifact.showResolved', label: 'Show resolved' },
			{ field: 'workspace.onboarding', label: 'Consecration', help: 'run the wizard again' },
		],
	},
	{
		id: 'secrets',
		label: 'Secrets',
		Icon: KeyRound,
		description: 'Stronghold vault — workspace, project and pkg scoped secrets.',
		fields: [],
	},
	{
		id: 'integrations',
		label: 'Integrations',
		Icon: Plug,
		description: 'Supabase, connectors, iyke MCP and bridge API keys.',
		fields: [],
	},
	{
		id: 'people',
		label: 'People & devices',
		Icon: Users,
		description: 'Who this workspace is shared with, and on which devices.',
		fields: [],
	},
	{
		id: 'storage',
		label: 'Storage & backup',
		Icon: HardDrive,
		description: 'Screenshot destination, caches, backup and restore, danger zone.',
		fields: [{ field: 'storage.screenshotDirectory', label: 'Screenshot directory' }],
	},
	{
		id: 'about',
		label: 'Updates & about',
		Icon: Info,
		description: 'Version, channel, update policy, licences and attribution.',
		fields: [
			{ field: 'about.updates.autoCheck', label: 'Check for updates automatically' },
			{ field: 'about.updates.autoInstallApp', label: 'Auto-install shell updates' },
			{ field: 'about.updates.autoInstallPkgs', label: 'Auto-install package updates' },
		],
	},
];

export const SETTINGS_NAV_GROUPS: ReadonlyArray<{
	label: string;
	ids: readonly SettingsSectionId[];
}> = [
	{ label: 'Workspace', ids: ['appearance', 'projects', 'engines', 'workspace'] },
	{ label: 'Access', ids: ['secrets', 'integrations', 'people'] },
	{ label: 'System', ids: ['storage', 'about'] },
];

export function settingsSection(id: string | undefined): SettingsSectionMeta {
	return SETTINGS_SECTIONS.find((section) => section.id === id) ?? SETTINGS_SECTIONS[0];
}

export function settingsIykeLine(sectionId: string, scope: SettingsScopeId): string {
	return `iyke settings open ${sectionId}${scope === 'project' ? ' --scope project' : ''}`;
}

interface SettingsNavProps {
	activeId: SettingsSectionId;
	search: string;
	onSearchChange: (value: string) => void;
	onSelect: (id: SettingsSectionId) => void;
}

export function SettingsNav({ activeId, search, onSearchChange, onSelect }: SettingsNavProps) {
	return (
		<nav
			aria-label="Settings sections"
			className="flex h-full w-[200px] shrink-0 flex-col border-r border-border-soft bg-[var(--bg-sunken)]"
		>
			<div className="border-b border-border-soft p-2">
				<Input
					type="search"
					value={search}
					onChange={(e) => onSearchChange(e.target.value)}
					placeholder="Search settings"
					aria-label="Search settings"
					className="h-7 bg-background font-mono text-xs"
				/>
			</div>
			<div className="flex-1 overflow-y-auto px-2 py-3">
				{SETTINGS_NAV_GROUPS.map((group) => (
					<div key={group.label} className="mb-4">
						<div className="px-2 pb-1 font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
							{group.label}
						</div>
						{group.ids.map((id) => {
							const section = settingsSection(id);
							const Icon = section.Icon;
							const active = id === activeId && !search.trim();
							return (
								<button
									key={id}
									type="button"
									role="tab"
									aria-selected={active}
									data-nav={id}
									onClick={() => onSelect(id)}
									className={cn(
										'mb-0.5 flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-xs transition-colors',
										'font-mono outline-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary',
										active
											? 'bg-accent text-accent-foreground'
											: 'text-muted-foreground hover:bg-accent/50 hover:text-foreground'
									)}
								>
									<Icon className="h-3.5 w-3.5 shrink-0" />
									<span className="truncate">{section.label}</span>
								</button>
							);
						})}
					</div>
				))}
			</div>
		</nav>
	);
}
