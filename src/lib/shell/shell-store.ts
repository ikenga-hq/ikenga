import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import { useIkengaStore } from '@/lib/ikenga/theme-store';
import {
	readSettingsFile,
	watchSettings,
	writeSettingsField,
	writeSettingsFields,
} from '@/lib/settings/client';
import type {
	SettingsFieldValueMap,
	SettingsFileResult,
	SettingsOnboarding,
	SettingsPersonalField,
	SettingsWriteEntry,
	SettingsWriteOptions,
} from '@/lib/settings/types';
import { scopedPersistName } from '@/lib/window/window-context';
import {
	type Project,
	projectGetActive,
	projectList,
	projectSetActive,
	settingsGet,
	settingsSet,
} from '@/lib/tauri-cmd';

// ─── Tauri-backed settings_kv mirror keys ─────────────────────────────────
//
// Zustand's `persist` middleware keeps these in localStorage for instant
// first-paint hydration; settings_kv (migration 0013) is the durable copy
// that survives "Clear local data" and can later back cross-device sync.
// Frontend hydrates from Tauri at boot (hydrateSettingsFromRust) and
// write-throughs on every relevant setter.

const KV_DEFAULT_ENGINE = 'agent.defaultEngineId';
const KV_CLAUDE_WATCH = 'claude.watchEnabled';
const KV_ONBOARDING = 'onboarding.state';
const KV_USER_NAME = 'user.name';
const KV_UPDATES_AUTO_CHECK = 'updates.autoCheck';
const KV_UPDATES_AUTO_INSTALL_APP = 'updates.autoInstallApp';
const KV_UPDATES_AUTO_INSTALL_PKGS = 'updates.autoInstallPkgs';
const KV_SHELL_MIGRATION = 'settings.migrations.shell-v17';

// Set true while pulling values from Rust into the store so the
// subscribe-based onboarding mirror doesn't push them straight back.
let suppressKv = false;
let settingsWriteQueue: Promise<void> = Promise.resolve();
let settingsHydrationQueue: Promise<void> = Promise.resolve();
let settingsHydrationGeneration = 0;
function currentAppearance() {
	const state = useIkengaStore.getState();
	return {
		theme: state.theme,
		mode: state.mode,
		density: state.density,
		tintStrength: state.tintStrength,
	};
}

function enqueueSettingsTask(task: () => Promise<unknown>): Promise<void> {
	const next = settingsWriteQueue.catch(() => {}).then(task);
	settingsWriteQueue = next.then(
		() => undefined,
		() => undefined
	);
	return next.then(() => undefined);
}

function enqueueSettingsWrite(
	label: string,
	task: () => Promise<unknown>,
	rollback: () => void,
): void {
	const run = async () => {
		let lastError: unknown;
		for (let attempt = 0; attempt < 2; attempt += 1) {
			try {
				await task();
				return;
			} catch (error) {
				lastError = error;
			}
		}
		console.error(`[settings] write failed for ${label}:`, lastError);
		const previousSuppress = suppressKv;
		suppressKv = true;
		try {
			rollback();
		} finally {
			suppressKv = previousSuppress;
		}
	};
	void enqueueSettingsTask(run).catch(() => {});
}

function kvSet(key: string, value: unknown, rollback: () => void = () => {}): void {
	if (suppressKv) return;
	enqueueSettingsWrite(key, () => settingsSet(key, JSON.stringify(value)), rollback);
}

function queueSettingsField<K extends SettingsPersonalField>(
	field: K,
	value: SettingsFieldValueMap[K],
	rollback: () => void,
): void {
	enqueueSettingsWrite(
		field,
		() =>
			writeSettingsField({
				scope: 'personal',
				field,
				value,
			} as SettingsWriteOptions),
		rollback,
	);
}

function enqueueSettingsHydration(task: (ticket: number) => Promise<void>): Promise<void> {
	const ticket = ++settingsHydrationGeneration;
	const next = settingsHydrationQueue.catch(() => {}).then(() => task(ticket));
	settingsHydrationQueue = next;
	return next;
}

async function readSettingsFileWithRetry(
	projectId?: string | null,
): Promise<SettingsFileResult | null> {
	for (let attempt = 0; attempt < 2; attempt += 1) {
		try {
			return await readSettingsFile({ scope: 'project', projectId: projectId ?? null });
		} catch {
			if (attempt === 1) return null;
			await new Promise((resolve) => setTimeout(resolve, 25));
		}
	}
	return null;
}

function recordAt(value: unknown, path: readonly string[]): unknown {
	let current = value;
	for (const key of path) {
		if (!current || typeof current !== 'object' || Array.isArray(current)) return undefined;
		current = (current as Record<string, unknown>)[key];
	}
	return current;
}

function stringAt(value: unknown, path: readonly string[]): string | undefined {
	const result = recordAt(value, path);
	return typeof result === 'string' ? result : undefined;
}

function booleanAt(value: unknown, path: readonly string[]): boolean | undefined {
	const result = recordAt(value, path);
	return typeof result === 'boolean' ? result : undefined;
}

function normalizeExplorerSections(value: unknown): ExplorerSectionState[] {
	if (!Array.isArray(value)) return createDefaultExplorerSections();
	const sections: ExplorerSectionState[] = [];
	for (const item of value) {
		if (!item || typeof item !== 'object') continue;
		const record = item as Record<string, unknown>;
		if (typeof record.id !== 'string' || typeof record.source !== 'string') continue;
		if (typeof record.order !== 'number' || typeof record.collapsed !== 'boolean') continue;
		sections.push({
			id: record.id,
			source: record.source,
			order: record.order,
			collapsed: record.collapsed,
		});
	}
	return sections.length > 0 ? sections : createDefaultExplorerSections();
}

function normalizeOnboarding(value: unknown): OnboardingState {
	const defaults = createDefaultOnboardingState();
	if (!value || typeof value !== 'object' || Array.isArray(value)) return defaults;
	const record = value as Record<string, unknown>;
	const steps =
		record.steps && typeof record.steps === 'object' && !Array.isArray(record.steps)
			? { ...defaults.steps, ...(record.steps as Record<string, OnboardingStepRecord>) }
			: defaults.steps;
	return {
		...defaults,
		...record,
		steps,
		loreGlossSeen: Array.isArray(record.loreGlossSeen)
			? record.loreGlossSeen.filter((term): term is string => typeof term === 'string')
			: [],
	} as OnboardingState;
}

function projectCanWriteSettings(project: Project | undefined): boolean {
	return project != null && project.archived_at == null && project.root_path != null;
}

function rootSettingsEntry(
	projectId: string,
	value: string[],
	project: Project | undefined,
): SettingsWriteEntry {
	if (projectCanWriteSettings(project)) {
		return { scope: 'project', field: 'projects.extraRoots', value, projectId };
	}
	return { scope: 'personal', field: 'projects.extraRoots', value };
}

function settingsFileFields(state: ShellState): SettingsWriteEntry[] {
	const appearance = currentAppearance();
	const userName = state.userName;
	const defaultEngineId = state.defaultEngineId;
	const claudeBrowserMode = state.claudeBrowserMode;
	const claudeWatchEnabled = state.claudeWatchEnabled;
	const sidebarCollapsed = state.sidebarCollapsed;
	const explorerSections = state.explorerSections;
	const onboarding = state.onboarding as SettingsOnboarding;
	const updatesAutoCheck = state.updatesAutoCheck;
	const updatesAutoInstallApp = state.updatesAutoInstallApp;
	const updatesAutoInstallPkgs = state.updatesAutoInstallPkgs;
	return [
		{ scope: 'personal', field: 'appearance.theme', value: appearance.theme },
		{ scope: 'personal', field: 'appearance.mode', value: appearance.mode },
		{ scope: 'personal', field: 'appearance.density', value: appearance.density },
		{ scope: 'personal', field: 'appearance.tintStrength', value: appearance.tintStrength },
		{
			scope: 'personal',
			field: 'workspace.userName',
			value: userName,
		},
		{
			scope: 'personal',
			field: 'engines.defaultEngineId',
			value: defaultEngineId,
		},
		{
			scope: 'personal',
			field: 'workspace.claudeBrowserMode',
			value: claudeBrowserMode,
		},
		{
			scope: 'personal',
			field: 'workspace.claudeWatchEnabled',
			value: claudeWatchEnabled,
		},
		{
			scope: 'personal',
			field: 'workspace.sidebarCollapsed',
			value: sidebarCollapsed,
		},
		{
			scope: 'personal',
			field: 'workspace.explorerSections',
			value: explorerSections,
		},
		{
			scope: 'personal',
			field: 'workspace.onboarding',
			value: onboarding,
		},
		{
			scope: 'personal',
			field: 'about.updates.autoCheck',
			value: updatesAutoCheck,
		},
		{
			scope: 'personal',
			field: 'about.updates.autoInstallApp',
			value: updatesAutoInstallApp,
		},
		{
			scope: 'personal',
			field: 'about.updates.autoInstallPkgs',
			value: updatesAutoInstallPkgs,
		},
	];
}

function legacySettingsFields(state: ShellState): SettingsWriteEntry[] {
	const fields = settingsFileFields(state);
	const projectRoots = state.projectExtraRoots;
	const activeProjectId = state.activeProjectId || 'default';
	let activeWritten = false;
	for (const [projectId, roots] of Object.entries(projectRoots)) {
		const project = state.projects.find((entry) => entry.id === projectId);
		if (project?.archived_at != null) continue;
		const isActive = projectId === activeProjectId;
		if (!project && !isActive && projectId !== 'default') continue;
		if (!isActive && !projectCanWriteSettings(project)) continue;
		fields.push(
			rootSettingsEntry(
				projectId,
				isActive
					? dedupeRoots([...roots, ...state.carriedRoots])
					: dedupeRoots(roots),
				project,
			),
		);
		if (isActive) activeWritten = true;
	}
	if (!activeWritten) {
		const project = state.projects.find((entry) => entry.id === activeProjectId);
		if (project?.archived_at == null) {
			fields.push(
				rootSettingsEntry(
					activeProjectId,
					dedupeRoots([...(projectRoots[activeProjectId] ?? []), ...state.carriedRoots]),
					project,
				),
			);
		}
	}
	return fields;
}

async function writeMissingLegacyState(
	state: ShellState,
	snapshot: SettingsFileResult,
): Promise<void> {
	const missing: SettingsWriteEntry[] = [];
	for (const field of legacySettingsFields(state)) {
		const document =
			field.scope === 'project'
				? (await readSettingsFileWithRetry(field.projectId))?.project
				: snapshot.personal;
		if (recordAt(document, field.field.split('.')) === undefined) missing.push(field);
	}
	const activeProject = state.projects.find((entry) => entry.id === state.activeProjectId);
	if (
		projectCanWriteSettings(activeProject) &&
		missing.some((field) => field.scope === 'project' && field.field === 'projects.extraRoots') &&
		recordAt(snapshot.personal, ['projects', 'extraRoots']) !== undefined
	) {
		missing.push({ scope: 'personal', field: 'projects.extraRoots', value: [], remove: true });
	}
	if (missing.length > 0) await writeSettingsFields(missing, state.activeProjectId);
}

function persistWorkspaceField<K extends SettingsPersonalField>(
	field: K,
	value: SettingsFieldValueMap[K],
	rollback: () => void = () => {}
): void {
	queueSettingsField(field, value, rollback);
}

// ─── Modes (G-STATE, persist v16) ──────────────────────────────────────────
//
// Phase 1 of the shell UX rearchitecture narrows the activity rail to four
// first-class modes: Project (the workbench — files, artifacts, sessions and
// package views all live under it), Chi (agents), Ngwa (Claude config +
// packages) and Settings. Package views no longer own a mode of their own.
// Contract: plans/shell-ux-rearchitecture/drafts/g-state.md §1.
export type CoreMode = 'project' | 'chi' | 'ngwa' | 'settings';
export type ActivityMode = CoreMode;

// Runtime list of every valid activity mode — the single source of truth that
// must stay in lockstep with the `CoreMode` union above. Consumed by the store
// migration, `normalizeMode` and the iyke control listener's `/iyke/mode`
// allow-list, so none of them can drift behind the union.
export const ACTIVITY_MODES: readonly CoreMode[] = Object.freeze([
	'project',
	'chi',
	'ngwa',
	'settings',
]);

export const DEFAULT_MODE: CoreMode = 'project';

/** True for one of the four v16 modes. */
export function isCoreMode(m: unknown): m is CoreMode {
	return typeof m === 'string' && (ACTIVITY_MODES as readonly string[]).includes(m);
}

/** Mode names the pre-v16 rail wrote (v10–v15 core modes and v14 dynamic
 *  `pkg:<id>` modes). Only the `/iyke/mode` bridge still accepts them, for
 *  one release, normalized through `normalizeMode` (g-state.md §5). */
const PRE_V16_MODE_NAMES: readonly string[] = Object.freeze([
	'app',
	'files',
	'sessions',
	'artifact-grid',
	'pkgs',
]);

/** True for a pre-v16 mode name that `normalizeMode` maps onto a CoreMode. */
export function isPreV16ModeName(m: unknown): m is string {
	return typeof m === 'string' && (PRE_V16_MODE_NAMES.includes(m) || m.startsWith('pkg:'));
}

/**
 * The v15 → v16 mode mapping (g-state.md §4). Total: any input, including
 * garbage, yields a CoreMode.
 *   app · files · sessions · artifact-grid · pkg:<id> → project
 *   pkgs · ngwa → ngwa
 *   settings → settings; project · chi unchanged
 *   missing, non-string, anything else → project
 */
export function normalizeMode(m: unknown): CoreMode {
	if (isCoreMode(m)) return m;
	if (m === 'pkgs') return 'ngwa';
	return DEFAULT_MODE;
}

// ─── Active project (derived) ─────────────────────────────────────────────
export interface ActiveProject {
	/** === activeProjectId (Rust-owned, never persisted). */
	id: string;
	/** From `projects[]`; null for the path-less `default` row. */
	root_path: string | null;
	/** dedupe([...projectExtraRoots[id] ?? [], ...carriedRoots]), order kept. */
	extra_roots: string[];
}

/** Trim, drop empty / non-string entries, dedupe on the exact trimmed string
 *  keeping first occurrence. Non-array input yields `[]`. */
export function dedupeRoots(input: unknown): string[] {
	if (!Array.isArray(input)) return [];
	const out: string[] = [];
	for (const raw of input) {
		if (typeof raw !== 'string') continue;
		const trimmed = raw.trim();
		if (!trimmed || out.includes(trimmed)) continue;
		out.push(trimmed);
	}
	return out;
}

function computeActiveProject(
	id: string,
	projects: readonly Project[],
	projectExtraRoots: Record<string, string[]>,
	carriedRoots: readonly string[],
	prev?: ActiveProject
): ActiveProject {
	const root_path = projects.find((p) => p.id === id)?.root_path ?? null;
	const extra_roots = dedupeRoots([...(projectExtraRoots[id] ?? []), ...carriedRoots]);
	// Keep the previous reference when nothing changed, so selectors on
	// `activeProject` don't re-render on unrelated project-list refreshes.
	if (
		prev &&
		prev.id === id &&
		prev.root_path === root_path &&
		prev.extra_roots.length === extra_roots.length &&
		prev.extra_roots.every((r, i) => r === extra_roots[i])
	) {
		return prev;
	}
	return { id, root_path, extra_roots };
}

// ─── Explorer sections ────────────────────────────────────────────────────
export type BuiltinExplorerSectionId =
	| 'files'
	| 'artifacts'
	| 'sessions'
	| 'ngwa-project'
	| 'automations'
	| 'todos'
	| 'scratchpads'
	| 'views';

export interface ExplorerSectionState {
	/** BuiltinExplorerSectionId, or `${pkg_id}:${section_id}` from Phase 4. */
	id: string;
	/** 'shell' or the contributing pkg id. */
	source: 'shell' | string;
	/** Integer, ascending; built-ins 0..7 by default. */
	order: number;
	collapsed: boolean;
}

const DEFAULT_EXPLORER_LAYOUT: ReadonlyArray<readonly [BuiltinExplorerSectionId, boolean]> = [
	['files', false],
	['artifacts', false],
	['sessions', false],
	['ngwa-project', true],
	['automations', false],
	['todos', true],
	['scratchpads', true],
	['views', true],
];

/** Spec §6.1 defaults: Files · Artifacts · Sessions · Automations open; the
 *  rest collapsed. Fresh objects on every call. */
export function createDefaultExplorerSections(): ExplorerSectionState[] {
	return DEFAULT_EXPLORER_LAYOUT.map(([id, collapsed], order) => ({
		id,
		source: 'shell',
		order,
		collapsed,
	}));
}

// ─── Companion ────────────────────────────────────────────────────────────
export type CompanionTarget =
	| { kind: 'session'; session_id: string }
	| { kind: 'new'; engine_id: string | null }
	| { kind: 'persistent'; engine_id: string | null };

// ─── v15 backup / rollback (g-state.md §4) ────────────────────────────────
const SHELL_STORE_BASE_KEY = 'shell-store';
export const V15_BACKUP_SUFFIX = '.__v15_backup';

function safeLocalStorage(): Storage | null {
	try {
		return typeof localStorage === 'undefined' ? null : localStorage;
	} catch {
		return null;
	}
}

/** Write the incoming pre-v16 payload as the exact Zustand
 *  envelope `JSON.stringify({ state, version })`. Skipped when the key already
 *  exists. Never throws — a failed backup must not block the migration. */
function writeV15Backup(persisted: unknown, version: number): void {
	try {
		const storage = safeLocalStorage();
		if (!storage) return;
		const key = `${scopedPersistName(SHELL_STORE_BASE_KEY)}${V15_BACKUP_SUFFIX}`;
		if (storage.getItem(key) !== null) return;
		// Serialising here, before migrate mutates anything, *is* the deep
		// clone: the string is a snapshot of the incoming payload.
		storage.setItem(key, JSON.stringify({ state: persisted, version }));
	} catch (err) {
		console.warn('[shell-store] v15 backup failed; migrating anyway:', err);
	}
}

/**
 * Rollback helper: put the `__v15_backup` blob back under the live key and
 * remove the backup. Run it (DevTools or `iyke eval`) on the v16 build, then
 * quit and launch the v15 build — it reads its own byte-identical blob.
 * Returns false (and changes nothing) when no backup exists.
 */
export function restoreV15Backup(
	storage: Storage = localStorage,
	key: string = scopedPersistName(SHELL_STORE_BASE_KEY)
): boolean {
	const backup = storage.getItem(`${key}${V15_BACKUP_SUFFIX}`);
	if (backup === null) return false;
	storage.setItem(key, backup);
	storage.removeItem(`${key}${V15_BACKUP_SUFFIX}`);
	return true;
}

// ─── Onboarding wizard state (Phase 3 scaffold) ──────────────────────────
//
// First-run setup. Persisted alongside the rest of shell-store so the user
// only sees the wizard once unless they explicitly re-run from Settings.
// Step bodies are filled in by Phase 4+; Phase 3 just lays down the shape +
// migration + chrome.

export type OnboardingStepId =
	| 'welcome'
	| 'agent'
	| 'roots'
	| 'packages'
	| 'connectors' // dynamic; substeps are derived (Phase 5)
	| 'scaffolding'
	| 'appearance'
	| 'summary';

export type OnboardingStatus = 'pending' | 'in_progress' | 'completed' | 'skipped';

export interface OnboardingStepRecord<P = unknown> {
	status: OnboardingStatus;
	completedAt?: number;
	/** Step-local snapshot of what the user chose. Schema per step. */
	payload?: P;
}

export interface OnboardingState {
	/** Bump to re-prompt for new mandatory steps in a future release. */
	version: number;
	startedAt: number | null;
	completedAt: number | null;
	/** First-run vs. re-run from Settings. */
	mode: 'first_run' | 'edit';
	/** Index of the currently active step (within the ordered step list). */
	activeIndex: number;
	steps: Record<OnboardingStepId, OnboardingStepRecord>;
	selectedAgentId: string | null;
	/** Tier-1 lore terms (Chi, Obi, Alusi…) the user has already seen the
	 * first-contact gloss for. Suppresses repeat tooltips. Per
	 * design/shell/05-lore-and-nomenclature.md §2 / wizard-spec.md §13.4. */
	loreGlossSeen: string[];
}

// Canonical step order. Source of truth for activeIndex math + stepper UI.
export const ONBOARDING_STEPS: readonly OnboardingStepId[] = Object.freeze([
	'welcome',
	'agent',
	'roots',
	'packages',
	'connectors',
	'scaffolding',
	'appearance',
	'summary',
]);

// Steps the user is allowed to skip. Welcome/Summary are not skippable
// (they're framing), agent/roots/packages are required to actually use
// the shell. The rest are optional.
export const OPTIONAL_ONBOARDING_STEPS: ReadonlySet<OnboardingStepId> = new Set<OnboardingStepId>([
	'connectors',
	'scaffolding',
	'appearance',
]);

/** Bump when the OnboardingState shape changes in a way that needs migration. */
export const ONBOARDING_STATE_VERSION = 2;

function freshStepRecord(): OnboardingStepRecord {
	return { status: 'pending' };
}

export function createDefaultOnboardingState(): OnboardingState {
	return {
		version: ONBOARDING_STATE_VERSION,
		startedAt: null,
		completedAt: null,
		mode: 'first_run',
		activeIndex: 0,
		steps: ONBOARDING_STEPS.reduce(
			(acc, id) => {
				acc[id] = freshStepRecord();
				return acc;
			},
			{} as Record<OnboardingStepId, OnboardingStepRecord>
		),
		selectedAgentId: null,
		loreGlossSeen: [],
	};
}

export interface SettingsRecovery {
	userName?: string;
	defaultEngineId?: string | null;
	updatesAutoCheck?: boolean;
	updatesAutoInstallApp?: boolean;
	updatesAutoInstallPkgs?: boolean;
	claudeWatchEnabled?: boolean;
	claudeBrowserMode?: 'layered' | 'roots';
	sidebarCollapsed?: boolean;
	projectExtraRoots?: Record<string, string[]>;
	carriedRoots?: string[];
	explorerSections?: ExplorerSectionState[];
	onboarding?: OnboardingState;
	appearance?: {
		theme: 'A' | 'B' | 'C';
		mode: 'light' | 'dark' | 'system';
		density: 'compact' | 'comfortable' | 'spacious';
		tintStrength: 'off' | 'subtle' | 'strong';
	};
	fileRoots?: unknown[];
	claudeProjectRoots?: unknown[];
}

interface ShellState {
	/** Always one of ACTIVITY_MODES — legacy names are normalized on the way in. */
	activeMode: CoreMode;
	setActiveMode: (m: CoreMode) => void;

	// ─── Active project + roots (G-STATE) ────────────────────────────────
	/** Derived, kept in sync by the store; a stable reference, so it is safe
	 *  as a selector result. Never persisted. */
	activeProject: ActiveProject;
	/** Persisted; extra roots per project id. */
	projectExtraRoots: Record<string, string[]>;
	/** Persisted; v15 fileRoots ∪ claudeProjectRoots, written once by migrate. */
	carriedRoots: string[];
	/** Trims, dedupes, recomputes `activeProject`. */
	setProjectExtraRoots: (projectId: string, roots: string[]) => void;

	// ─── Explorer sections (G-STATE) ─────────────────────────────────────
	explorerSections: ExplorerSectionState[];
	settingsRecovery: SettingsRecovery | null;
	setExplorerSectionCollapsed: (id: string, collapsed: boolean) => void;
	/** Swap with the neighbour above (-1) or below (+1); orders renumbered 0..n-1. */
	moveExplorerSection: (id: string, delta: -1 | 1) => void;

	// ─── Companion (G-STATE) ─────────────────────────────────────────────
	/** Not persisted — session ids are dead after a restart. */
	companion: { activeTarget: CompanionTarget };
	setCompanionTarget: (t: CompanionTarget) => void;

	// ─── Sidebar visibility ──────────────────────────────────────────────
	// Lives here rather than as local state in `workspace.tsx` because two
	// independent surfaces drive it: the ⌘B shortcut and the activity-bar
	// rail (clicking the already-active item collapses/reopens it). Both
	// must read and write the same value or they desync — pressing ⌘B then
	// clicking the active rail item would otherwise need two clicks to
	// reopen. Persisted with the rest of the store, so a collapsed sidebar
	// survives a restart the way it does in other editors.
	sidebarCollapsed: boolean;
	setSidebarCollapsed: (v: boolean) => void;
	toggleSidebar: () => void;

	// ─── User identity ───────────────────────────────────────────────────
	// The user's display name — used by the daily-address greeting and any
	// surface that needs to address the owner directly. Optional; empty
	// string means "not yet provided" (the welcome step asks for it but
	// allows skipping). Stored separately from Tauri-side identity so the
	// shell can render it without an OS-API roundtrip.
	userName: string;
	setUserName: (name: string) => void;

	// ─── Default engine agent ────────────────────────────────────────────
	// Which engine adapter pkg drives terminal sessions. Mirrors the
	// agent step's `selectedAgentId` after onboarding completes; left
	// null when the user picks offline mode.
	defaultEngineId: string | null;
	setDefaultEngineId: (id: string | null) => void;

	// ─── Auto-update preferences ─────────────────────────────────────────
	// `updatesAutoCheck` gates the boot + 6h poll for BOTH the app binary
	// (useUpdater) and the pkg registry. `updatesAutoInstallApp` is OFF by
	// default — the app relaunches the user, so a binary update stays a
	// one-click action from the banner/About unless explicitly opted in.
	// `updatesAutoInstallPkgs` is ON by default — pkgs are sandboxed and
	// hot-reload in place (no relaunch), so silent background updates are
	// low-surprise.
	updatesAutoCheck: boolean;
	setUpdatesAutoCheck: (v: boolean) => void;
	updatesAutoInstallApp: boolean;
	setUpdatesAutoInstallApp: (v: boolean) => void;
	updatesAutoInstallPkgs: boolean;
	setUpdatesAutoInstallPkgs: (v: boolean) => void;

	claudeWatchEnabled: boolean;
	setClaudeWatchEnabled: (enabled: boolean) => void;

	/** Which Claude config browser surface is active. 'layered' uses the
	 * 4-tier discovery (Phase 4); 'roots' is the legacy 2-tier scan kept
	 * around as a fallback. UI-only preference, persisted via Zustand. */
	claudeBrowserMode: 'layered' | 'roots';
	setClaudeBrowserMode: (mode: 'layered' | 'roots') => void;

	// ─── Onboarding ──────────────────────────────────────────────────────
	onboarding: OnboardingState;
	/** Mark the wizard as having started (sets `startedAt` if not already set,
	 * flips current step to in_progress). Idempotent. */
	startOnboarding: (mode?: OnboardingState['mode']) => void;
	setOnboardingPayload: <P>(stepId: OnboardingStepId, payload: P) => void;
	setSelectedAgentId: (id: string | null) => void;
	markOnboardingStepCompleted: (stepId: OnboardingStepId) => void;
	markOnboardingStepSkipped: (stepId: OnboardingStepId) => void;
	setOnboardingActiveIndex: (idx: number) => void;
	/** Re-enter the wizard at a specific step in edit mode (from Settings). */
	enterOnboardingEdit: (stepId: OnboardingStepId) => void;
	/** Mark every step as completed and stamp completedAt — called from the
	 * summary step's "Open workspace" terminal action. */
	finishOnboarding: () => void;
	/** Reset the wizard to a fresh first-run state — used by Settings
	 * "Start over". */
	resetOnboarding: () => void;
	/** Record that the user has seen the first-contact gloss for a Tier-1
	 * lore term (Chi, Obi, Alusi…). Idempotent. */
	markGlossSeen: (term: string) => void;

	/** Pull durable settings from Rust (settings_kv) and overwrite local
	 * state. If settings_kv is empty, push the currently-persisted Zustand
	 * snapshot in once so existing users carry over. Called once at app
	 * boot from `main.tsx`; safe to call multiple times. Rejects silently
	 * in non-Tauri test environments. */
	hydrateSettingsFromRust: () => Promise<void>;

	// ─── Projects (Phase 0 — first-class) ─────────────────────────────────
	// The Rust side owns the durable list (migration 0015) and the active
	// project id (settings_kv `shell.activeProjectId`). Persistence is not
	// duplicated in Zustand — these fields live in memory only, hydrated
	// at boot from `refreshProjects`.
	projects: Project[];
	activeProjectId: string;
	/** Switch the active project. Updates Rust side first, then refreshes
	 *  the local list. The Rust emit of `projects:active-changed` is what
	 *  drives TanStack invalidation in the workspace-level listener. */
	setActiveProject: (id: string) => Promise<void>;
	/** Pull the project list + active project id from Rust. Safe to call
	 *  multiple times; rejects silently in non-Tauri test environments. */
	refreshProjects: () => Promise<void>;
}

/** Store keys kept out of the persisted blob (g-state.md §3). */
const NOT_PERSISTED: ReadonlySet<string> = new Set([
	'projects',
	'activeProjectId',
	'activeProject',
	'companion',
	'userName',
	'defaultEngineId',
	'updatesAutoCheck',
	'updatesAutoInstallApp',
	'updatesAutoInstallPkgs',
	'claudeWatchEnabled',
	'claudeBrowserMode',
	'sidebarCollapsed',
	'projectExtraRoots',
	'carriedRoots',
	'explorerSections',
	'onboarding',
]);

function clampActiveIndex(idx: number): number {
	if (Number.isNaN(idx) || idx < 0) return 0;
	if (idx > ONBOARDING_STEPS.length - 1) return ONBOARDING_STEPS.length - 1;
	return idx;
}

// Exposed for unit tests. Zustand's `persist` middleware doesn't surface
// the migrate fn through a clean public API, so we hoist the logic into
// a named helper and reference it from both the `persist({ migrate })`
// option and the tests.
export function migrateShellStore(persisted: unknown, version: number): unknown {
	const p = (persisted ?? {}) as Partial<ShellState> & {
		activeMode?: string;
		agent_onboarded?: boolean;
		selected_agent_id?: string | null;
		onboarding?: Partial<OnboardingState>;
	};

	// v7 carry-over, repointed at v16: every stored activeMode — whatever
	// version it was written by — goes through the one total mapping in
	// `normalizeMode` (g-state.md §4). Dead pre-v7 modes, the v10–v15 rail
	// modes and v14 `pkg:<id>` modes all land on a v16 CoreMode; missing or
	// non-string values become 'project'.
	(p as { activeMode?: unknown }).activeMode = normalizeMode(
		(p as { activeMode?: unknown }).activeMode
	);

	// v8: build OnboardingState from defaults + legacy keys if present.
	if (!p.onboarding) {
		const next = createDefaultOnboardingState();
		const hadLegacyAgent = p.agent_onboarded === true;
		const legacyAgentId =
			typeof p.selected_agent_id === 'string' && p.selected_agent_id.length > 0
				? p.selected_agent_id
				: null;

		if (hadLegacyAgent || legacyAgentId) {
			if (hadLegacyAgent) {
				next.steps.agent = {
					status: 'completed',
					completedAt: Date.now(),
					payload: legacyAgentId ? { agentId: legacyAgentId } : undefined,
				};
			}
			if (legacyAgentId) {
				next.selectedAgentId = legacyAgentId;
			}
		}
		p.onboarding = next;
	} else {
		// Defensive: legacy installs may have a partial onboarding blob
		// from a hand-edit. Merge over defaults so missing step records
		// get filled in.
		const defaults = createDefaultOnboardingState();
		const merged: OnboardingState = {
			...defaults,
			...(p.onboarding as OnboardingState),
			steps: {
				...defaults.steps,
				...((p.onboarding as OnboardingState).steps ?? {}),
			},
			// v2: backfill loreGlossSeen for installs that predate the lore overlay.
			loreGlossSeen: Array.isArray((p.onboarding as OnboardingState).loreGlossSeen)
				? (p.onboarding as OnboardingState).loreGlossSeen
				: [],
		};
		merged.activeIndex = clampActiveIndex(merged.activeIndex);
		p.onboarding = merged;
	}

	// Drop the legacy flat keys so they don't get reused on next load.
	delete p.agent_onboarded;
	delete p.selected_agent_id;

	// v9: seed canonical default engine id from the onboarding payload when
	// missing on disk. (v15 removed the telemetry consent seeding that used
	// to live here.)
	const px = p as Partial<ShellState> & {
	onboarding?: SettingsOnboarding;

		defaultEngineId?: string | null;
		chatAdapterId?: string | null;
	};
	if (typeof px.defaultEngineId === 'undefined') {
		px.defaultEngineId = px.chatAdapterId ?? px.onboarding?.selectedAgentId ?? null;
	}
	delete (p as unknown as Record<string, unknown>).chatAdapterId;

	// v15: the telemetry consent step and its persisted state are gone.
	// Drop any stale keys from localStorage/settings_kv hydration so the
	// store snapshot stays clean and doesn't try to re-introduce the field.
	delete (p as unknown as Record<string, unknown>).telemetryConsent;
	if (p.onboarding) {
		delete ((p.onboarding as unknown as { steps?: Record<string, unknown> }).steps ?? {}).telemetry;
	}

	// v16 (G-STATE): v15 roots were global and the active project is unknown
	// at migrate time (`projects` loads async after rehydrate), so the union
	// of fileRoots + claudeProjectRoots is carried globally and surfaces in
	// `activeProject.extra_roots` whichever project is active. The legacy
	// fields themselves are left untouched (WP-05 retires them).
	if (version < 16) {
		const rec = p as unknown as Record<string, unknown>;
		const legacyFileRoots = Array.isArray(rec.fileRoots) ? rec.fileRoots : [];
		const legacyClaudeRoots = Array.isArray(rec.claudeProjectRoots) ? rec.claudeProjectRoots : [];
		rec.carriedRoots = dedupeRoots([...legacyFileRoots, ...legacyClaudeRoots]);
		rec.projectExtraRoots = {};
		if (!Array.isArray(rec.explorerSections)) {
			rec.explorerSections = createDefaultExplorerSections();
		}
		// Derived / session-scoped — never read from a blob.
		delete rec.activeProject;
		delete rec.companion;
	}

	const rec = p as unknown as Record<string, unknown>;
	if (
		version < 17 ||
		['userName', 'defaultEngineId', 'updatesAutoCheck', 'updatesAutoInstallApp', 'updatesAutoInstallPkgs', 'claudeWatchEnabled', 'claudeBrowserMode', 'sidebarCollapsed', 'projectExtraRoots', 'carriedRoots', 'explorerSections', 'onboarding', 'fileRoots', 'claudeProjectRoots'].some((key) =>
			Object.hasOwn(rec, key)
		)
	) {
		rec.carriedRoots = dedupeRoots(rec.carriedRoots);
		rec.projectExtraRoots =
			rec.projectExtraRoots &&
			typeof rec.projectExtraRoots === 'object' &&
			!Array.isArray(rec.projectExtraRoots)
				? Object.fromEntries(
						Object.entries(rec.projectExtraRoots as Record<string, unknown>).map(([id, roots]) => [
							id,
							dedupeRoots(roots),
						])
					)
				: {};
		rec.explorerSections = normalizeExplorerSections(rec.explorerSections);
		if (!rec.settingsRecovery) {
			rec.settingsRecovery = {
				userName: typeof rec.userName === 'string' ? rec.userName : undefined,
				defaultEngineId:
					typeof rec.defaultEngineId === 'string' || rec.defaultEngineId === null
						? rec.defaultEngineId
						: undefined,
				updatesAutoCheck:
					typeof rec.updatesAutoCheck === 'boolean' ? rec.updatesAutoCheck : undefined,
				updatesAutoInstallApp:
					typeof rec.updatesAutoInstallApp === 'boolean' ? rec.updatesAutoInstallApp : undefined,
				updatesAutoInstallPkgs:
					typeof rec.updatesAutoInstallPkgs === 'boolean' ? rec.updatesAutoInstallPkgs : undefined,
				claudeWatchEnabled:
					typeof rec.claudeWatchEnabled === 'boolean' ? rec.claudeWatchEnabled : undefined,
				claudeBrowserMode:
					rec.claudeBrowserMode === 'layered' || rec.claudeBrowserMode === 'roots'
						? rec.claudeBrowserMode
						: undefined,
				sidebarCollapsed:
					typeof rec.sidebarCollapsed === 'boolean' ? rec.sidebarCollapsed : undefined,
				projectExtraRoots: rec.projectExtraRoots as Record<string, string[]>,
				carriedRoots: rec.carriedRoots as string[],
				explorerSections: rec.explorerSections as ExplorerSectionState[],
				onboarding: rec.onboarding as OnboardingState,
				appearance: currentAppearance(),
				fileRoots: Array.isArray(rec.fileRoots) ? rec.fileRoots : undefined,
				claudeProjectRoots: Array.isArray(rec.claudeProjectRoots)
					? rec.claudeProjectRoots
					: undefined,
			} satisfies SettingsRecovery;
		}
	}

	return p;
}

/**
 * Persist `merge`: overlay the (migrated) blob on the initial state, then
 * re-derive what is never persisted. Also runs on a fresh profile (with
 * `persisted === undefined`), where it yields exactly the §2 defaults.
 */
function mergeShellState(persisted: unknown, current: ShellState): ShellState {
	const blob =
		persisted && typeof persisted === 'object'
			? { ...(persisted as Record<string, unknown>) }
			: ({} as Record<string, unknown>);
	// Rust-owned, derived or session-scoped: the in-memory value wins.
	delete blob.projects;
	delete blob.activeProjectId;
	delete blob.activeProject;
	delete blob.companion;
	const next = { ...current, ...blob } as ShellState;
	const recovery = next.settingsRecovery;
	if (recovery) {
		if (!Object.hasOwn(blob, 'userName') && recovery.userName !== undefined) {
			next.userName = recovery.userName;
		}
		if (!Object.hasOwn(blob, 'defaultEngineId') && recovery.defaultEngineId !== undefined) {
			next.defaultEngineId = recovery.defaultEngineId;
		}
		if (!Object.hasOwn(blob, 'updatesAutoCheck') && recovery.updatesAutoCheck !== undefined) {
			next.updatesAutoCheck = recovery.updatesAutoCheck;
		}
		if (
			!Object.hasOwn(blob, 'updatesAutoInstallApp') &&
			recovery.updatesAutoInstallApp !== undefined
		) {
			next.updatesAutoInstallApp = recovery.updatesAutoInstallApp;
		}
		if (
			!Object.hasOwn(blob, 'updatesAutoInstallPkgs') &&
			recovery.updatesAutoInstallPkgs !== undefined
		) {
			next.updatesAutoInstallPkgs = recovery.updatesAutoInstallPkgs;
		}
		if (!Object.hasOwn(blob, 'claudeWatchEnabled') && recovery.claudeWatchEnabled !== undefined) {
			next.claudeWatchEnabled = recovery.claudeWatchEnabled;
		}
		if (!Object.hasOwn(blob, 'claudeBrowserMode') && recovery.claudeBrowserMode !== undefined) {
			next.claudeBrowserMode = recovery.claudeBrowserMode;
		}
		if (!Object.hasOwn(blob, 'sidebarCollapsed') && recovery.sidebarCollapsed !== undefined) {
			next.sidebarCollapsed = recovery.sidebarCollapsed;
		}
		if (!Object.hasOwn(blob, 'projectExtraRoots') && recovery.projectExtraRoots !== undefined) {
			next.projectExtraRoots = Object.fromEntries(
				Object.entries(recovery.projectExtraRoots).map(([id, roots]) => [
					id,
					dedupeRoots(roots),
				])
			);
		}
		if (!Object.hasOwn(blob, 'carriedRoots') && recovery.carriedRoots !== undefined) {
			next.carriedRoots = dedupeRoots(recovery.carriedRoots);
		}
		if (!Object.hasOwn(blob, 'explorerSections') && recovery.explorerSections !== undefined) {
			next.explorerSections = normalizeExplorerSections(recovery.explorerSections);
		}
		if (!Object.hasOwn(blob, 'onboarding') && recovery.onboarding !== undefined) {
			next.onboarding = normalizeOnboarding(recovery.onboarding);
		}
	}
	next.activeMode = normalizeMode(next.activeMode);
	next.carriedRoots = dedupeRoots(next.carriedRoots);
	if (
		!next.projectExtraRoots ||
		typeof next.projectExtraRoots !== 'object' ||
		Array.isArray(next.projectExtraRoots)
	) {
		next.projectExtraRoots = {};
	} else {
		next.projectExtraRoots = Object.fromEntries(
			Object.entries(next.projectExtraRoots).map(([id, roots]) => [id, dedupeRoots(roots)])
		);
	}
	if (!Array.isArray(next.explorerSections)) {
		next.explorerSections = createDefaultExplorerSections();
	}
	next.activeProject = computeActiveProject(
		next.activeProjectId,
		next.projects,
		next.projectExtraRoots,
		next.carriedRoots,
		current.activeProject
	);
	return next;
}

export const useShellStore = create<ShellState>()(
	persist(
		(set, get) => ({
			activeMode: DEFAULT_MODE,
			// Typed CoreMode-only since WP-03. Still routed through the total
			// `normalizeMode` so an untyped caller (a bridge payload cast to
			// CoreMode) can never store anything but one of the four modes.
			setActiveMode: (m) => set({ activeMode: normalizeMode(m) }),

			// Before `refreshProjects` resolves this is the seed `default` row
			// with no root; `merge` re-derives it on rehydrate.
			activeProject: { id: 'default', root_path: null, extra_roots: [] },
			projectExtraRoots: {},
			carriedRoots: [],
			setProjectExtraRoots: (projectId, roots) => {
				const s = get();
				const project = s.projects.find((entry) => entry.id === projectId);
				if (project?.archived_at != null) return;
				const previousRoots = s.projectExtraRoots[projectId] ?? [];
				const nextRoots = dedupeRoots(roots);
				const projectExtraRoots = { ...s.projectExtraRoots, [projectId]: nextRoots };
				set({
					projectExtraRoots,
					activeProject: computeActiveProject(
						s.activeProjectId,
						s.projects,
						projectExtraRoots,
						s.carriedRoots,
						s.activeProject
					),
				});
				enqueueSettingsWrite(
					'projects.extraRoots',
					() =>
						writeSettingsField(rootSettingsEntry(projectId, nextRoots, project)),
					() => {
						const current = get();
						if (JSON.stringify(current.projectExtraRoots[projectId] ?? []) !== JSON.stringify(nextRoots)) return;
						const restored = { ...current.projectExtraRoots, [projectId]: previousRoots };
						set({
							projectExtraRoots: restored,
							activeProject: computeActiveProject(
								current.activeProjectId,
								current.projects,
								restored,
								current.carriedRoots,
								current.activeProject
							),
						});
					}
				);
			},

			explorerSections: createDefaultExplorerSections(),
			settingsRecovery: null,
			setExplorerSectionCollapsed: (id, collapsed) => {
				const previous = get().explorerSections;
				const next = previous.map((sec) =>
					sec.id === id && sec.collapsed !== collapsed ? { ...sec, collapsed } : sec
				);
				set({ explorerSections: next });
				persistWorkspaceField('workspace.explorerSections', next, () => {
					if (get().explorerSections === next) set({ explorerSections: previous });
				});
			},
			moveExplorerSection: (id, delta) => {
				const current = get();
				const previous = current.explorerSections;
				const sorted = [...previous].sort((a, b) => a.order - b.order);
				const idx = sorted.findIndex((sec) => sec.id === id);
				const target = idx + delta;
				if (idx < 0 || target < 0 || target >= sorted.length) return;
				const moved = sorted[idx]!;
				sorted[idx] = sorted[target]!;
				sorted[target] = moved;
				const next = sorted.map((sec, order) =>
					sec.order === order ? sec : { ...sec, order }
				);
				set({ explorerSections: next });
				persistWorkspaceField('workspace.explorerSections', next, () => {
					if (get().explorerSections === next) set({ explorerSections: previous });
				});
			},

			companion: { activeTarget: { kind: 'new', engine_id: null } },
			setCompanionTarget: (activeTarget) => set({ companion: { activeTarget } }),

			sidebarCollapsed: false,
			setSidebarCollapsed: (sidebarCollapsed) => {
				const previous = get().sidebarCollapsed;
				set({ sidebarCollapsed });
				persistWorkspaceField('workspace.sidebarCollapsed', sidebarCollapsed, () => {
					if (get().sidebarCollapsed === sidebarCollapsed) set({ sidebarCollapsed: previous });
				});
			},
			toggleSidebar: () => {
				const previous = get().sidebarCollapsed;
				const sidebarCollapsed = !previous;
				set({ sidebarCollapsed });
				persistWorkspaceField('workspace.sidebarCollapsed', sidebarCollapsed, () => {
					if (get().sidebarCollapsed === sidebarCollapsed) set({ sidebarCollapsed: previous });
				});
			},

			userName: '',
			setUserName: (userName) => {
				const previous = get().userName;
				const trimmed = userName.trim();
				set({ userName: trimmed });
				kvSet(KV_USER_NAME, trimmed, () => {
					if (get().userName === trimmed) set({ userName: previous });
				});
			},

			defaultEngineId: null,
			setDefaultEngineId: (defaultEngineId) => {
				const previous = get().defaultEngineId;
				set({ defaultEngineId });
				kvSet(KV_DEFAULT_ENGINE, defaultEngineId, () => {
					if (get().defaultEngineId === defaultEngineId) set({ defaultEngineId: previous });
				});
			},

			updatesAutoCheck: true,
			setUpdatesAutoCheck: (updatesAutoCheck) => {
				const previous = get().updatesAutoCheck;
				set({ updatesAutoCheck });
				kvSet(KV_UPDATES_AUTO_CHECK, updatesAutoCheck, () => {
					if (get().updatesAutoCheck === updatesAutoCheck) set({ updatesAutoCheck: previous });
				});
			},
			updatesAutoInstallApp: false,
			setUpdatesAutoInstallApp: (updatesAutoInstallApp) => {
				const previous = get().updatesAutoInstallApp;
				set({ updatesAutoInstallApp });
				kvSet(KV_UPDATES_AUTO_INSTALL_APP, updatesAutoInstallApp, () => {
					if (get().updatesAutoInstallApp === updatesAutoInstallApp) set({ updatesAutoInstallApp: previous });
				});
			},
			updatesAutoInstallPkgs: true,
			setUpdatesAutoInstallPkgs: (updatesAutoInstallPkgs) => {
				const previous = get().updatesAutoInstallPkgs;
				set({ updatesAutoInstallPkgs });
				kvSet(KV_UPDATES_AUTO_INSTALL_PKGS, updatesAutoInstallPkgs, () => {
					if (get().updatesAutoInstallPkgs === updatesAutoInstallPkgs) set({ updatesAutoInstallPkgs: previous });
				});
			},

			claudeWatchEnabled: true,
			setClaudeWatchEnabled: (claudeWatchEnabled) => {
				const previous = get().claudeWatchEnabled;
				set({ claudeWatchEnabled });
				kvSet(KV_CLAUDE_WATCH, claudeWatchEnabled, () => {
					if (get().claudeWatchEnabled === claudeWatchEnabled) set({ claudeWatchEnabled: previous });
				});
			},

			claudeBrowserMode: 'layered',
			setClaudeBrowserMode: (claudeBrowserMode) => {
				const previous = get().claudeBrowserMode;
				set({ claudeBrowserMode });
				persistWorkspaceField('workspace.claudeBrowserMode', claudeBrowserMode, () => {
					if (get().claudeBrowserMode === claudeBrowserMode) set({ claudeBrowserMode: previous });
				});
			},

			// ─── Onboarding actions ────────────────────────────────────────
			onboarding: createDefaultOnboardingState(),

			startOnboarding: (mode = 'first_run') =>
				set((state) => {
					const ob = state.onboarding;
					const startedAt = ob.startedAt ?? Date.now();
					const activeId = ONBOARDING_STEPS[clampActiveIndex(ob.activeIndex)]!;
					const currentRecord = ob.steps[activeId];
					const nextSteps =
						currentRecord.status === 'pending'
							? { ...ob.steps, [activeId]: { ...currentRecord, status: 'in_progress' as const } }
							: ob.steps;
					return {
						onboarding: {
							...ob,
							mode,
							startedAt,
							steps: nextSteps,
						},
					};
				}),

			setOnboardingPayload: (stepId, payload) =>
				set((state) => {
					const ob = state.onboarding;
					const existing = ob.steps[stepId];
					return {
						onboarding: {
							...ob,
							steps: {
								...ob.steps,
								[stepId]: { ...existing, payload },
							},
						},
					};
				}),

			setSelectedAgentId: (id) =>
				set((state) => ({ onboarding: { ...state.onboarding, selectedAgentId: id } })),

			markOnboardingStepCompleted: (stepId) =>
				set((state) => {
					const ob = state.onboarding;
					const existing = ob.steps[stepId];
					return {
						onboarding: {
							...ob,
							steps: {
								...ob.steps,
								[stepId]: {
									...existing,
									status: 'completed',
									completedAt: Date.now(),
								},
							},
						},
					};
				}),

			markOnboardingStepSkipped: (stepId) =>
				set((state) => {
					const ob = state.onboarding;
					// Only optional steps may be skipped. Caller is expected to gate
					// this in the UI; the action enforces it defensively.
					if (!OPTIONAL_ONBOARDING_STEPS.has(stepId)) return state;
					const existing = ob.steps[stepId];
					return {
						onboarding: {
							...ob,
							steps: {
								...ob.steps,
								[stepId]: {
									...existing,
									status: 'skipped',
									completedAt: Date.now(),
								},
							},
						},
					};
				}),

			setOnboardingActiveIndex: (idx) =>
				set((state) => ({
					onboarding: {
						...state.onboarding,
						activeIndex: clampActiveIndex(idx),
					},
				})),

			enterOnboardingEdit: (stepId) =>
				set((state) => {
					const idx = ONBOARDING_STEPS.indexOf(stepId);
					if (idx < 0) return state;
					return {
						onboarding: {
							...state.onboarding,
							mode: 'edit',
							activeIndex: idx,
						},
					};
				}),

			finishOnboarding: () =>
				set((state) => ({
					onboarding: {
						...state.onboarding,
						completedAt: Date.now(),
						activeIndex: ONBOARDING_STEPS.length - 1,
					},
				})),

			resetOnboarding: () => set({ onboarding: createDefaultOnboardingState() }),

			markGlossSeen: (term) =>
				set((state) => {
					const ob = state.onboarding;
					const key = term.toLowerCase();
					const seen = ob.loreGlossSeen ?? [];
					if (seen.some((t) => t.toLowerCase() === key)) return state;
					return {
						onboarding: { ...ob, loreGlossSeen: [...seen, term] },
					};
				}),

			// ─── Projects ─────────────────────────────────────────────────
			// Boot value is the bootstrap default — Rust always seeds a
			// `default` row in migration 0015, and the active id is the
			// same until the user picks something else. `refreshProjects`
			// at boot replaces both fields with the authoritative copy.
			projects: [],
			activeProjectId: 'default',
			setActiveProject: async (id: string) => {
				// Optimistic local update so the activity-bar indicator and
				// any indicator-derived UI flip instantly. Rust emits the
				// `projects:active-changed` event which the workspace-level
				// listener uses to invalidate project-scoped queries.
				const prev = get().activeProjectId;
				if (prev === id) return;
				const withActive = (activeProjectId: string) => {
					const s = get();
					return {
						activeProjectId,
						activeProject: computeActiveProject(
							activeProjectId,
							s.projects,
							s.projectExtraRoots,
							s.carriedRoots,
							s.activeProject
						),
					};
				};
				set(withActive(id));
				try {
					await projectSetActive(id);
				} catch (err) {
					// Surface the failure — rolling this back silently strands the
					// user on the previous project (often the path-less `default`),
					// which is exactly what makes new terminals open in `~`.
					console.warn('[shell-store] projectSetActive failed:', err);
					// Roll back the optimistic flip — but only if nobody
					// flipped again in the meantime.
					if (get().activeProjectId === id) {
						set(withActive(prev));
					}
					throw err;
				}
				try {
					await get().hydrateSettingsFromRust();
				} catch (error) {
					console.error('[shell-store] project settings hydration failed:', error);
				}
			},
			refreshProjects: async () => {
				try {
					const [list, active] = await Promise.all([projectList(true), projectGetActive()]);
					const s = get();
					set({
						projects: list,
						activeProjectId: active.id,
						activeProject: computeActiveProject(
							active.id,
							list,
							s.projectExtraRoots,
							s.carriedRoots,
							s.activeProject
						),
					});
					await get().hydrateSettingsFromRust().catch((error) => {
						console.error('[shell-store] project settings hydration failed:', error);
					});
				} catch (err) {
					// Tauri unavailable (test env / pre-setup boot) — but a real
					// failure here leaves the store on the seed `default` project
					// (null root_path → activeProjectCwd() falls back to `~`), so
					// log it rather than swallowing silently.
					console.warn('[shell-store] refreshProjects failed:', err);
				}
			},

			hydrateSettingsFromRust: () =>
				enqueueSettingsHydration(async (hydrationTicket) => {
					if (hydrationTicket !== settingsHydrationGeneration) return;
					if (get().settingsRecovery && get().projects.length === 0) return;
					const expectedProjectId = get().activeProjectId;
					const firstSnapshot = await readSettingsFileWithRetry();
					if (!firstSnapshot) return;
					let snapshot: SettingsFileResult = firstSnapshot;
					if (hydrationTicket !== settingsHydrationGeneration) return;
					if (get().activeProjectId !== expectedProjectId) return;
					if (snapshot.projectId !== null && snapshot.projectId !== expectedProjectId) return;

					try {
						await enqueueSettingsTask(async () => {
							if (get().settingsRecovery) {
								await writeMissingLegacyState(get(), snapshot);
							}
							if (get().settingsRecovery) {
								await settingsSet(KV_SHELL_MIGRATION, JSON.stringify(true));
								const verified = (await settingsGet(KV_SHELL_MIGRATION)) === 'true';
								if (!verified) throw new Error('shell settings migration marker was not verified');
								const refreshedSnapshot = await readSettingsFileWithRetry();
								if (!refreshedSnapshot) throw new Error('settings reread failed');
								snapshot = refreshedSnapshot;
								if (get().activeProjectId !== expectedProjectId) {
									throw new Error('settings project changed during handoff');
								}
								if (snapshot.projectId !== null && snapshot.projectId !== expectedProjectId) {
									throw new Error('settings project mismatch during handoff');
								}
								if (get().settingsRecovery) {
									set((state) => {
										const next = { ...state, carriedRoots: [], settingsRecovery: null };
										delete (next as unknown as Record<string, unknown>).fileRoots;
										delete (next as unknown as Record<string, unknown>).claudeProjectRoots;
										return next;
									});
								}
							}
						});
					} catch {
						return;
					}
					if (hydrationTicket !== settingsHydrationGeneration) return;
					if (get().activeProjectId !== expectedProjectId) return;
					if (snapshot.projectId !== null && snapshot.projectId !== expectedProjectId) return;

					const effective = snapshot.effective;
					const current = get();
					const projectId = snapshot.projectId ?? current.activeProjectId;
					const roots = recordAt(effective, ['projects', 'extraRoots']);
					const projectExtraRoots = { ...current.projectExtraRoots };
					projectExtraRoots[projectId] = Array.isArray(roots) ? dedupeRoots(roots) : [];
					const onboarding = recordAt(effective, ['workspace', 'onboarding']);
					const explorerSections = recordAt(effective, ['workspace', 'explorerSections']);
					const next: Partial<ShellState> = {
						userName: stringAt(effective, ['workspace', 'userName']) ?? '',
						defaultEngineId:
							(recordAt(effective, ['engines', 'defaultEngineId']) as string | null | undefined) ??
							null,
						updatesAutoCheck: booleanAt(effective, ['about', 'updates', 'autoCheck']) ?? true,
						updatesAutoInstallApp:
							booleanAt(effective, ['about', 'updates', 'autoInstallApp']) ?? false,
						updatesAutoInstallPkgs:
							booleanAt(effective, ['about', 'updates', 'autoInstallPkgs']) ?? true,
						claudeWatchEnabled: booleanAt(effective, ['workspace', 'claudeWatchEnabled']) ?? true,
						claudeBrowserMode:
							stringAt(effective, ['workspace', 'claudeBrowserMode']) === 'roots'
								? 'roots'
								: 'layered',
						sidebarCollapsed: booleanAt(effective, ['workspace', 'sidebarCollapsed']) ?? false,
						explorerSections: normalizeExplorerSections(explorerSections),
						onboarding: normalizeOnboarding(onboarding),
					};
					const activeProject = computeActiveProject(
						projectId,
						current.projects,
						projectExtraRoots,
						[],
						current.activeProject
					);
					suppressKv = true;
					try {
						set({ ...next, carriedRoots: [], projectExtraRoots, activeProject });
					} finally {
						suppressKv = false;
					}
					await useIkengaStore.getState().hydrateAppearanceFromRust().catch(() => {});
				}),
			}),
				// Bump version when ActivityMode union or persisted shape changes.
		// v5: mail/outbox/studio promoted to CoreMode (then v7 narrowed).
		// v6: added claudeProjectRoots / claudeWatchEnabled.
		// v7: strip-down — CoreMode narrowed to {app, files, sessions, settings};
		//     migrate snaps any stale persisted activeMode (mail/outbox/studio/
		//     agents/mini-app names) → 'app' so users coming from the legacy
		//     shell don't crash on an invalid persisted union value.
		// v8: onboarding wizard scaffold — added `onboarding` slice. Migrates
		//     legacy `agent_onboarded` / `selected_agent_id` keys (from the
		//     predecessor onboarding plan) into the new OnboardingState.
		// v9: canonical default engine id. (v15 removed the telemetry consent
		//     seeding that used to live here.)
		// v10: widen CoreMode with 'pkgs' for the registry browser activity-bar
		//     entry. Migrate keeps the same valid-set check, just widened.
		// v11: widen CoreMode with 'artifact-grid' for the artifact-grid
		//     activity-bar entry (projects-and-artifact-wizard plan §B2).
		// v12: lore overlay — OnboardingState gains `loreGlossSeen: string[]`
		//     to suppress first-contact gloss tooltips after acknowledgement.
		//     Migrate backfills `[]` for existing installs.
		// v13: widen CoreMode with 'ngwa' (Ngwa Claude-config activity-bar
		//     mode, ⌘6). Migrate keeps the same valid-set check, just widened;
		//     no persisted users could already hold 'ngwa', so it's additive.
		// v14: ActivityMode widened with dynamic `pkg:<id>` modes — every app
		//     pkg now owns its own activity-bar mode instead of borrowing 'app'
		//     and clobbering the main nav. Migrate preserves persisted pkg
		//     modes; a stale one (pkg uninstalled) reconciles → 'app' at runtime
		//     in the activity bar. Additive — no persisted user holds a pkg mode.
		// v15: removed telemetry consent and the telemetry onboarding step.
		//     Migrate drops any persisted `telemetryConsent` and `onboarding.steps.telemetry`.
		// v16: G-STATE (plans/shell-ux-rearchitecture/drafts/g-state.md). CoreMode
		//     narrowed to project|chi|ngwa|settings via `normalizeMode`; v15
		//     fileRoots ∪ claudeProjectRoots carried as `carriedRoots`; new
		//     `projectExtraRoots` + `explorerSections`. The incoming pre-v16
		//     payload is kept under `<key>.__v15_backup` for one release
		//     (`restoreV15Backup` is the rollback path).
		{
			// Window-namespaced (plans/multi-window WP-05): the primary `main`
			// window keeps the bare `shell-store` key (existing persisted state
			// preserved); a detached window gets a `::<label>` suffix so its
			// `activeMode`/onboarding writes don't clobber the primary's via the
			// localStorage that all same-origin Tauri windows share (research 03).
			name: scopedPersistName(SHELL_STORE_BASE_KEY),
			version: 17,
			migrate: (persisted, version) => {
				// Backup first, before any v16 mutation of the payload.
				if (version < 16) writeV15Backup(persisted, version);
				return migrateShellStore(persisted, version) as ShellState;
			},
			merge: (persisted, current) => mergeShellState(persisted, current),
			// `projects` + `activeProjectId` are owned by Rust (migration 0015)
			// and re-pulled every boot via `refreshProjects`. They must NOT be
			// persisted here — a stale localStorage snapshot (e.g. a path-less
			// `default` left over from an old session) would rehydrate over the
			// authoritative Rust copy and make `activeProjectCwd()` fall back to
			// `~`, so new terminals spawn in $HOME instead of the active
			// project root. Keep them out of the persisted blob.
			// v16: `activeProject` is derived (rebuilt on rehydrate) and
			// `companion` is session-scoped (its session ids die with the app).
			partialize: (state) =>
				Object.fromEntries(
					Object.entries(state).filter(([k]) => !NOT_PERSISTED.has(k))
				) as Partial<ShellState>,
		}
	)
);

// Mirror the onboarding slice into settings_kv whenever it changes. Covers
// every onboarding mutator (startOnboarding, setOnboardingPayload,
// setSelectedAgentId, mark*StepCompleted/Skipped, setOnboardingActiveIndex,
// enterOnboardingEdit, finishOnboarding, resetOnboarding) without each
// mutator having to opt in. Suppressed during `hydrateSettingsFromRust` so
// we don't push the value we just pulled.
useShellStore.subscribe((state, prev) => {
	if (state.onboarding !== prev.onboarding) {
		kvSet(KV_ONBOARDING, state.onboarding, () => {
			if (useShellStore.getState().onboarding === state.onboarding) {
				useShellStore.setState({ onboarding: prev.onboarding });
			}
		});
	}
});

if (
	typeof window !== 'undefined' &&
	('__TAURI_INTERNALS__' in window || '__TAURI__' in window)
) {
	void watchSettings(async () => {
		await useShellStore.getState().hydrateSettingsFromRust();
		await useIkengaStore.getState().hydrateAppearanceFromRust();
	}).catch(() => {});
}
