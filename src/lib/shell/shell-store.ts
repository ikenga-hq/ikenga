import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import { scopedPersistName } from '@/lib/window/window-context';
import {
	fsRootsAdd,
	fsRootsList,
	fsRootsRemove,
	fsRootsReset,
	type Project,
	projectGetActive,
	projectList,
	projectSetActive,
	settingsGetAll,
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
const KV_LEGACY_CHAT_ADAPTER = 'agent.chatAdapterId';
const KV_CLAUDE_ROOTS = 'claude.projectRoots';
const KV_CLAUDE_WATCH = 'claude.watchEnabled';
const KV_ONBOARDING = 'onboarding.state';
const KV_USER_NAME = 'user.name';
const KV_UPDATES_AUTO_CHECK = 'updates.autoCheck';
const KV_UPDATES_AUTO_INSTALL_APP = 'updates.autoInstallApp';
const KV_UPDATES_AUTO_INSTALL_PKGS = 'updates.autoInstallPkgs';

// Set true while pulling values from Rust into the store so the
// subscribe-based onboarding mirror doesn't push them straight back.
let suppressKv = false;

function kvSet(key: string, value: unknown): void {
	if (suppressKv) return;
	settingsSet(key, JSON.stringify(value)).catch(() => {
		// Tauri unavailable (test env / pre-setup) — localStorage is still
		// the in-page cache so the user's edit is not lost.
	});
}

function parseKv<T>(raw: string | undefined): T | undefined {
	if (raw == null) return undefined;
	try {
		return JSON.parse(raw) as T;
	} catch {
		return undefined;
	}
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

// Default file roots. Kept in sync with `src-tauri/src/fs_roots.rs::DEFAULT_ROOTS`;
// the Rust side is authoritative — these are only the seed values used by the
// onboarding wizard's "reset to defaults" affordance and the test harness.
// At runtime, `fileRoots` is hydrated from Rust on app boot (see
// `hydrateFileRootsFromRust`).
//
// Empty by design: a fresh install has no allowlist until the user adds a
// root via the onboarding wizard or Settings → Storage.
export const DEFAULT_FILE_ROOTS: readonly string[] = Object.freeze([]);

// Project roots scanned by the /claude config browser. Each root is a dir
// that contains a `.claude/` subfolder (agents/skills/commands/settings).
// Personal `~/.claude/` is always scanned in addition to these — it doesn't
// need to be listed. Empty by default; the user adds roots via onboarding
// step "roots" or Settings.
export const DEFAULT_CLAUDE_PROJECT_ROOTS: readonly string[] = Object.freeze([]);

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
	/** Persisted per profile, kept sorted by `order`. */
	explorerSections: ExplorerSectionState[];
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

	fileRoots: string[];
	addFileRoot: (path: string) => void;
	removeFileRoot: (path: string) => void;
	/** Replace `oldPath` with `newPath` (no-op if oldPath isn't present, or if
	 * the new path is empty / a duplicate of an existing entry). Used by the
	 * editable settings selectors. */
	updateFileRoot: (oldPath: string, newPath: string) => void;
	resetFileRoots: () => void;
	/** Pull the authoritative list from Rust (`fs_roots_list`) and overwrite
	 * local state. Called at app boot; safe to call multiple times. Rejects
	 * silently in non-Tauri test environments. */
	hydrateFileRootsFromRust: () => Promise<void>;

	claudeProjectRoots: string[];
	addClaudeProjectRoot: (path: string) => void;
	removeClaudeProjectRoot: (path: string) => void;
	updateClaudeProjectRoot: (oldPath: string, newPath: string) => void;
	resetClaudeProjectRoots: () => void;
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
		onboarding?: OnboardingState;
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
	next.activeMode = normalizeMode(next.activeMode);
	next.carriedRoots = dedupeRoots(next.carriedRoots);
	if (
		!next.projectExtraRoots ||
		typeof next.projectExtraRoots !== 'object' ||
		Array.isArray(next.projectExtraRoots)
	) {
		next.projectExtraRoots = {};
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
				const projectExtraRoots = { ...s.projectExtraRoots, [projectId]: dedupeRoots(roots) };
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
			},

			explorerSections: createDefaultExplorerSections(),
			setExplorerSectionCollapsed: (id, collapsed) =>
				set((s) => ({
					explorerSections: s.explorerSections.map((sec) =>
						sec.id === id && sec.collapsed !== collapsed ? { ...sec, collapsed } : sec
					),
				})),
			moveExplorerSection: (id, delta) =>
				set((s) => {
					const sorted = [...s.explorerSections].sort((a, b) => a.order - b.order);
					const idx = sorted.findIndex((sec) => sec.id === id);
					const target = idx + delta;
					if (idx < 0 || target < 0 || target >= sorted.length) return s;
					const moved = sorted[idx]!;
					sorted[idx] = sorted[target]!;
					sorted[target] = moved;
					return {
						explorerSections: sorted.map((sec, order) =>
							sec.order === order ? sec : { ...sec, order }
						),
					};
				}),

			companion: { activeTarget: { kind: 'new', engine_id: null } },
			setCompanionTarget: (activeTarget) => set({ companion: { activeTarget } }),

			sidebarCollapsed: false,
			setSidebarCollapsed: (sidebarCollapsed) => set({ sidebarCollapsed }),
			toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),

			userName: '',
			setUserName: (userName) => {
				const trimmed = userName.trim();
				set({ userName: trimmed });
				kvSet(KV_USER_NAME, trimmed);
			},

			defaultEngineId: null,
			setDefaultEngineId: (defaultEngineId) => {
				set({ defaultEngineId });
				kvSet(KV_DEFAULT_ENGINE, defaultEngineId);
			},

			updatesAutoCheck: true,
			setUpdatesAutoCheck: (updatesAutoCheck) => {
				set({ updatesAutoCheck });
				kvSet(KV_UPDATES_AUTO_CHECK, updatesAutoCheck);
			},
			updatesAutoInstallApp: false,
			setUpdatesAutoInstallApp: (updatesAutoInstallApp) => {
				set({ updatesAutoInstallApp });
				kvSet(KV_UPDATES_AUTO_INSTALL_APP, updatesAutoInstallApp);
			},
			updatesAutoInstallPkgs: true,
			setUpdatesAutoInstallPkgs: (updatesAutoInstallPkgs) => {
				set({ updatesAutoInstallPkgs });
				kvSet(KV_UPDATES_AUTO_INSTALL_PKGS, updatesAutoInstallPkgs);
			},

			fileRoots: [...DEFAULT_FILE_ROOTS],
			// All four mutators update local state optimistically for instant UI
			// feedback, then sync the authoritative list back from Rust. The
			// invoke promise is swallowed in non-Tauri test environments so the
			// existing unit tests (which never see a Tauri runtime) still pass.
			addFileRoot: (path) => {
				const trimmed = path.trim();
				if (!trimmed) return;
				if (!get().fileRoots.includes(trimmed)) {
					set({ fileRoots: [...get().fileRoots, trimmed] });
				}
				fsRootsAdd(trimmed)
					.then((next) => set({ fileRoots: next }))
					.catch(() => {});
			},
			removeFileRoot: (path) => {
				set({ fileRoots: get().fileRoots.filter((r) => r !== path) });
				fsRootsRemove(path)
					.then((next) => set({ fileRoots: next }))
					.catch(() => {});
			},
			updateFileRoot: (oldPath, newPath) => {
				const trimmed = newPath.trim();
				if (!trimmed || trimmed === oldPath) return;
				const cur = get().fileRoots;
				const idx = cur.indexOf(oldPath);
				if (idx < 0) return;
				// Don't allow renaming on top of another existing entry.
				if (cur.includes(trimmed)) return;
				const next = [...cur];
				next[idx] = trimmed;
				set({ fileRoots: next });
				// Rust has no atomic "rename" — sequence remove+add. If the
				// remove succeeds but add fails (e.g. invalid path), the user
				// sees a shorter list, matching the local state we already set.
				fsRootsRemove(oldPath)
					.then(() => fsRootsAdd(trimmed))
					.then((latest) => set({ fileRoots: latest }))
					.catch(() => {});
			},
			resetFileRoots: () => {
				set({ fileRoots: [...DEFAULT_FILE_ROOTS] });
				fsRootsReset()
					.then((next) => set({ fileRoots: next }))
					.catch(() => {});
			},
			hydrateFileRootsFromRust: async () => {
				try {
					const next = await fsRootsList();
					set({ fileRoots: next });
				} catch {
					// Test environment or pre-setup boot — keep the persisted
					// snapshot. Caller can retry.
				}
			},

			claudeProjectRoots: [...DEFAULT_CLAUDE_PROJECT_ROOTS],
			addClaudeProjectRoot: (path) => {
				const trimmed = path.trim();
				if (!trimmed) return;
				if (get().claudeProjectRoots.includes(trimmed)) return;
				const next = [...get().claudeProjectRoots, trimmed];
				set({ claudeProjectRoots: next });
				kvSet(KV_CLAUDE_ROOTS, next);
			},
			removeClaudeProjectRoot: (path) => {
				const next = get().claudeProjectRoots.filter((r) => r !== path);
				set({ claudeProjectRoots: next });
				kvSet(KV_CLAUDE_ROOTS, next);
			},
			updateClaudeProjectRoot: (oldPath, newPath) => {
				const trimmed = newPath.trim();
				if (!trimmed || trimmed === oldPath) return;
				const cur = get().claudeProjectRoots;
				const idx = cur.indexOf(oldPath);
				if (idx < 0) return;
				if (cur.includes(trimmed)) return;
				const next = [...cur];
				next[idx] = trimmed;
				set({ claudeProjectRoots: next });
				kvSet(KV_CLAUDE_ROOTS, next);
			},
			resetClaudeProjectRoots: () => {
				const next = [...DEFAULT_CLAUDE_PROJECT_ROOTS];
				set({ claudeProjectRoots: next });
				kvSet(KV_CLAUDE_ROOTS, next);
			},
			claudeWatchEnabled: true,
			setClaudeWatchEnabled: (claudeWatchEnabled) => {
				set({ claudeWatchEnabled });
				kvSet(KV_CLAUDE_WATCH, claudeWatchEnabled);
			},

			claudeBrowserMode: 'layered',
			setClaudeBrowserMode: (claudeBrowserMode) => {
				set({ claudeBrowserMode });
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
				} catch (err) {
					// Tauri unavailable (test env / pre-setup boot) — but a real
					// failure here leaves the store on the seed `default` project
					// (null root_path → activeProjectCwd() falls back to `~`), so
					// log it rather than swallowing silently.
					console.warn('[shell-store] refreshProjects failed:', err);
				}
			},

			hydrateSettingsFromRust: async () => {
				let all: Record<string, string> = {};
				try {
					all = await settingsGetAll();
				} catch {
					// Tauri unavailable (test env or pre-setup boot).
					return;
				}
				if (Object.keys(all).length === 0) {
					// First boot post-migration: seed settings_kv from whatever
					// localStorage hydrated us with so existing users carry over.
					const s = get();
					suppressKv = true;
					try {
						kvSet(KV_DEFAULT_ENGINE, s.defaultEngineId);
						kvSet(KV_CLAUDE_ROOTS, s.claudeProjectRoots);
						kvSet(KV_CLAUDE_WATCH, s.claudeWatchEnabled);
						kvSet(KV_ONBOARDING, s.onboarding);
						kvSet(KV_UPDATES_AUTO_CHECK, s.updatesAutoCheck);
						kvSet(KV_UPDATES_AUTO_INSTALL_APP, s.updatesAutoInstallApp);
						kvSet(KV_UPDATES_AUTO_INSTALL_PKGS, s.updatesAutoInstallPkgs);
					} finally {
						suppressKv = false;
					}
					return;
				}
				// Tauri has values — overwrite the relevant store slices.
				suppressKv = true;
				try {
					const next: Partial<ShellState> = {};
					const adapter = parseKv<string | null>(
						all[KV_DEFAULT_ENGINE] ?? all[KV_LEGACY_CHAT_ADAPTER]
					);
					if (adapter === null || typeof adapter === 'string') {
						next.defaultEngineId = adapter;
					}
					const roots = parseKv<string[]>(all[KV_CLAUDE_ROOTS]);
					if (Array.isArray(roots)) next.claudeProjectRoots = roots;
					const watch = parseKv<boolean>(all[KV_CLAUDE_WATCH]);
					if (typeof watch === 'boolean') next.claudeWatchEnabled = watch;
					const ob = parseKv<OnboardingState>(all[KV_ONBOARDING]);
					if (ob && typeof ob === 'object') {
						// Backfill loreGlossSeen for KV blobs persisted before v2.
						if (!Array.isArray(ob.loreGlossSeen)) ob.loreGlossSeen = [];
						next.onboarding = ob;
					}
					const userName = parseKv<string>(all[KV_USER_NAME]);
					if (typeof userName === 'string') next.userName = userName;
					const autoCheck = parseKv<boolean>(all[KV_UPDATES_AUTO_CHECK]);
					if (typeof autoCheck === 'boolean') next.updatesAutoCheck = autoCheck;
					const autoApp = parseKv<boolean>(all[KV_UPDATES_AUTO_INSTALL_APP]);
					if (typeof autoApp === 'boolean') next.updatesAutoInstallApp = autoApp;
					const autoPkgs = parseKv<boolean>(all[KV_UPDATES_AUTO_INSTALL_PKGS]);
					if (typeof autoPkgs === 'boolean') next.updatesAutoInstallPkgs = autoPkgs;
					set(next);
				} finally {
					suppressKv = false;
				}
			},
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
			version: 16,
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
		kvSet(KV_ONBOARDING, state.onboarding);
	}
});
