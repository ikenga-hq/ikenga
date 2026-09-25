export const SETTINGS_SCHEMA = 'urn:ikenga:settings:v1' as const;
export const SETTINGS_VERSION = 1 as const;

export type SettingsScope = 'personal' | 'project';
export type SettingsTheme = 'A' | 'B' | 'C';
export type SettingsMode = 'light' | 'dark' | 'system';
export type SettingsDensity = 'compact' | 'comfortable' | 'spacious';
export type SettingsTintStrength = 'off' | 'subtle' | 'strong';
export type SettingsAgentEnvironment = 'native' | 'wsl';
export type SettingsDefaultSink = 'auto' | 'terminal' | 'chi' | 'clipboard';
export type SettingsStackMode = 'collapsed' | 'expanded';
export type SettingsHandoff = 'attach' | 'keep' | 'ask';
export type SettingsLastAgentKind = 'claude' | 'codex' | 'gemini' | 'custom' | null;
export type SettingsArtifactSink =
	| 'inherit'
	| 'auto'
	| 'terminal'
	| 'chi'
	| 'clipboard'
	| `terminal:${string}`;

export interface SettingsShellProfile {
	id: string;
	label: string;
	icon: string;
	cmd: string[];
	isDefault: boolean;
	kind: string;
	distro: string | null;
}

export interface SettingsExplorerSection {
	id: string;
	source: string;
	order: number;
	collapsed: boolean;
}

export interface SettingsFolderOverride {
	defaultSink?: SettingsDefaultSink;
	stackMode?: SettingsStackMode;
}

export interface SettingsArtifactSettings {
	defaultSink: SettingsDefaultSink;
	stackMode: SettingsStackMode;
	terminalHandoff: SettingsHandoff;
	folderOverrides: Record<string, SettingsFolderOverride>;
	artifactSinkOverrides: Record<string, SettingsArtifactSink>;
	showResolved: Record<string, boolean>;
}

export interface SettingsOnboarding {
	version: number;
	startedAt: number | null;
	completedAt: number | null;
	mode: 'first_run' | 'edit';
	activeIndex: number;
	steps: Record<string, { status: string; completedAt?: number; payload?: unknown }>;
	selectedAgentId: string | null;
	loreGlossSeen: string[];
}

export interface SettingsWorkspace {
	userName: string;
	claudeBrowserMode: 'layered' | 'roots';
	claudeWatchEnabled: boolean;
	sidebarCollapsed: boolean;
	explorerSections: SettingsExplorerSection[];
	onboarding: Record<string, unknown>;
	artifact: SettingsArtifactSettings;
	lastAgent: { kind: SettingsLastAgentKind; customCommand: string | null };
}

export interface SettingsAppearance {
	theme: SettingsTheme;
	mode: SettingsMode;
	density: SettingsDensity;
	tintStrength: SettingsTintStrength;
}

export interface SettingsEngines {
	defaultEngineId: string | null;
	defaultShellId: string | null;
	customShellProfiles: SettingsShellProfile[];
	agentEnvironment: SettingsAgentEnvironment;
	agentWslDistro: string | null;
	resumeTerminals: boolean;
}

export interface SettingsProjects {
	extraRoots: string[];
}

export interface SettingsStorage {
	screenshotDirectory: string | null;
}

export interface SettingsUpdates {
	autoCheck: boolean;
	autoInstallApp: boolean;
	autoInstallPkgs: boolean;
}

export interface SettingsAbout {
	updates: SettingsUpdates;
}

export type SettingsSection = Record<string, unknown>;

export interface SettingsDocument {
	$schema: string;
	version: number;
	appearance: Partial<SettingsAppearance> & SettingsSection;
	projects: Partial<SettingsProjects> & SettingsSection;
	engines: Partial<SettingsEngines> & SettingsSection;
	workspace: Partial<SettingsWorkspace> & SettingsSection;
	secrets: SettingsSection;
	integrations: SettingsSection;
	people: SettingsSection;
	storage: Partial<SettingsStorage> & SettingsSection;
	about: Partial<SettingsAbout> & SettingsSection;
	[key: string]: unknown;
}

export interface SettingsFileResult {
	personal: SettingsDocument;
	project: SettingsDocument | null;
	effective: SettingsDocument;
	personalPath: string;
	projectPath: string | null;
	projectId: string | null;
	projectRoot: string | null;
	overrides: string[];
	personalPresent: boolean;
	projectPresent: boolean;
	scope: SettingsScope;
}

export interface SettingsChangeEvent {
	path: string;
}

export interface SettingsFieldValueMap {
	'appearance.theme': SettingsTheme;
	'appearance.mode': SettingsMode;
	'appearance.density': SettingsDensity;
	'appearance.tintStrength': SettingsTintStrength;
	'projects.extraRoots': string[];
	'engines.defaultEngineId': string | null;
	'engines.defaultShellId': string | null;
	'engines.customShellProfiles': SettingsShellProfile[];
	'engines.agentEnvironment': SettingsAgentEnvironment;
	'engines.agentWslDistro': string | null;
	'engines.resumeTerminals': boolean;
	'workspace.userName': string;
	'workspace.claudeBrowserMode': 'layered' | 'roots';
	'workspace.claudeWatchEnabled': boolean;
	'workspace.sidebarCollapsed': boolean;
	'workspace.explorerSections': SettingsExplorerSection[];
	'workspace.onboarding': SettingsOnboarding;
	'workspace.artifact': SettingsArtifactSettings;
	'workspace.artifact.defaultSink': SettingsDefaultSink;
	'workspace.artifact.stackMode': SettingsStackMode;
	'workspace.artifact.terminalHandoff': SettingsHandoff;
	'workspace.artifact.folderOverrides': Record<string, SettingsFolderOverride>;
	'workspace.artifact.artifactSinkOverrides': Record<string, SettingsArtifactSink>;
	'workspace.artifact.showResolved': Record<string, boolean>;
	'workspace.lastAgent': { kind: SettingsLastAgentKind; customCommand: string | null };
	'workspace.lastAgent.kind': SettingsLastAgentKind;
	'workspace.lastAgent.customCommand': string | null;
	'storage.screenshotDirectory': string | null;
	'about.updates': SettingsUpdates;
	'about.updates.autoCheck': boolean;
	'about.updates.autoInstallApp': boolean;
	'about.updates.autoInstallPkgs': boolean;
}

export type SettingsField = keyof SettingsFieldValueMap;

export type SettingsPersonalOnlyField =
	| 'workspace.userName'
	| 'workspace.claudeBrowserMode'
	| 'workspace.claudeWatchEnabled'
	| 'workspace.sidebarCollapsed'
	| 'workspace.explorerSections'
	| 'workspace.onboarding'
	| 'storage.screenshotDirectory'
	| 'about.updates'
	| 'about.updates.autoCheck'
	| 'about.updates.autoInstallApp'
	| 'about.updates.autoInstallPkgs';

export type SettingsProjectOnlyField =
	| 'workspace.lastAgent'
	| 'workspace.lastAgent.kind'
	| 'workspace.lastAgent.customCommand';

export type SettingsProjectCapableField = 'projects.extraRoots' | SettingsProjectOnlyField;
export type SettingsSharedField = Exclude<
	SettingsField,
	SettingsPersonalOnlyField | SettingsProjectCapableField
>;
export type SettingsPersonalField = SettingsPersonalOnlyField | SettingsSharedField;

type SettingsWriteBase<K extends SettingsField> = {
	field: K;
	value: SettingsFieldValueMap[K];
	remove?: boolean;
};

type PersonalSettingsWrite = {
	[K in SettingsPersonalField]: SettingsWriteBase<K> & {
		scope: 'personal';
		projectId?: never;
	};
}[SettingsPersonalField];

type ProjectSettingsWrite =
	| {
			[K in SettingsProjectOnlyField]: SettingsWriteBase<K> & {
				scope: 'project';
				projectId: string;
			};
	  }[SettingsProjectOnlyField]
	| {
			[K in SettingsSharedField]: SettingsWriteBase<K> & {
				scope: 'project';
				projectId: string;
			};
	  }[SettingsSharedField];

type RootSettingsWrite =
	| (SettingsWriteBase<'projects.extraRoots'> & {
			scope: 'personal';
			projectId?: never;
		})
	| (SettingsWriteBase<'projects.extraRoots'> & {
			scope: 'project';
			projectId: string;
		});

export type SettingsWriteOptions =
	| PersonalSettingsWrite
	| ProjectSettingsWrite
	| RootSettingsWrite;

export type SettingsWriteEntry = SettingsWriteOptions;

export const PERSONAL_ONLY_FIELDS = [
	'workspace.userName',
	'workspace.claudeBrowserMode',
	'workspace.claudeWatchEnabled',
	'workspace.sidebarCollapsed',
	'workspace.explorerSections',
	'workspace.onboarding',
	'storage.screenshotDirectory',
	'about.updates',
	'about.updates.autoCheck',
	'about.updates.autoInstallApp',
	'about.updates.autoInstallPkgs',
] as const satisfies readonly SettingsPersonalOnlyField[];

export const PROJECT_ONLY_FIELDS = [
	'projects.extraRoots',
	'workspace.lastAgent',
	'workspace.lastAgent.kind',
	'workspace.lastAgent.customCommand',
] as const satisfies readonly SettingsProjectCapableField[];

export const SETTINGS_DEFAULTS = {
	appearance: { theme: 'A', mode: 'dark', density: 'comfortable', tintStrength: 'subtle' },
	projects: { extraRoots: [] },
	engines: {
		defaultEngineId: null,
		defaultShellId: null,
		customShellProfiles: [],
		agentEnvironment: 'native',
		agentWslDistro: null,
		resumeTerminals: true,
	},
	workspace: {
		userName: '',
		claudeBrowserMode: 'layered',
		claudeWatchEnabled: true,
		sidebarCollapsed: false,
		artifact: {
			defaultSink: 'auto',
			stackMode: 'collapsed',
			terminalHandoff: 'ask',
			folderOverrides: {},
			artifactSinkOverrides: {},
			showResolved: {},
		},
		lastAgent: { kind: null, customCommand: null },
	},
	about: { updates: { autoCheck: true, autoInstallApp: false, autoInstallPkgs: true } },
	storage: { screenshotDirectory: null },
} as const;
