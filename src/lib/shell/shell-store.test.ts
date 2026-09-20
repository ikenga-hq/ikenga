// shell-store migration tests (onboarding history + v16 / G-STATE).
//
// The migrate fn is hoisted out of the persist middleware into a named
// export (`migrateShellStore`) so we can call it directly here without
// touching zustand's internal API surface.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
	ACTIVITY_MODES,
	createDefaultExplorerSections,
	type CoreMode,
	createDefaultOnboardingState,
	dedupeRoots,
	migrateShellStore,
	normalizeMode,
	ONBOARDING_STATE_VERSION,
	ONBOARDING_STEPS,
	type OnboardingState,
} from './shell-store';

describe('shell-store onboarding migration', () => {
	it('seeds a fresh OnboardingState when missing from persisted blob', () => {
		const migrated = migrateShellStore(
			{
				activeMode: 'app',
				fileRoots: ['~/royalti-co'],
				claudeProjectRoots: ['~/royalti-co'],
				claudeWatchEnabled: true,
			},
			7
		) as { onboarding: OnboardingState };

		expect(migrated.onboarding).toBeDefined();
		expect(migrated.onboarding.version).toBe(ONBOARDING_STATE_VERSION);
		expect(migrated.onboarding.completedAt).toBeNull();
		expect(migrated.onboarding.mode).toBe('first_run');
		for (const id of ONBOARDING_STEPS) {
			expect(migrated.onboarding.steps[id].status).toBe('pending');
		}
	});

	it('migrates legacy `agent_onboarded` + `selected_agent_id` into the agent step', () => {
		const migrated = migrateShellStore(
			{
				activeMode: 'app',
				agent_onboarded: true,
				selected_agent_id: 'claude-code',
			},
			7
		) as {
			onboarding: OnboardingState;
			agent_onboarded?: boolean;
			selected_agent_id?: string | null;
		};

		expect(migrated.onboarding.selectedAgentId).toBe('claude-code');
		expect(migrated.onboarding.steps.agent.status).toBe('completed');
		expect(typeof migrated.onboarding.steps.agent.completedAt).toBe('number');
		expect(migrated.onboarding.steps.agent.payload).toEqual({ agentId: 'claude-code' });

		// Legacy keys are scrubbed so they don't get reused.
		expect(migrated.agent_onboarded).toBeUndefined();
		expect(migrated.selected_agent_id).toBeUndefined();

		// Other steps stay pending — the legacy flag wasn't a full-wizard
		// completion signal.
		expect(migrated.onboarding.steps.welcome.status).toBe('pending');
		expect(migrated.onboarding.steps.summary.status).toBe('pending');
		expect(migrated.onboarding.completedAt).toBeNull();
	});

	it('migrates legacy `selected_agent_id` alone (no agent_onboarded flag)', () => {
		const migrated = migrateShellStore(
			{
				activeMode: 'app',
				selected_agent_id: 'codex',
			},
			7
		) as { onboarding: OnboardingState };

		expect(migrated.onboarding.selectedAgentId).toBe('codex');
		// Without the agent_onboarded flag, the step itself isn't marked done.
		expect(migrated.onboarding.steps.agent.status).toBe('pending');
	});

	it('merges over defaults when persisted blob already has a partial onboarding slice', () => {
		const partial = createDefaultOnboardingState();
		partial.steps.welcome = { status: 'completed', completedAt: 123 };
		// Intentionally omit a step from the persisted record to simulate a
		// future shape that adds a new step the user hasn't seen yet.
		const stepsMinusOne = { ...partial.steps };
		delete (stepsMinusOne as Record<string, unknown>).scaffolding;
		const blob = {
			activeMode: 'app',
			onboarding: { ...partial, steps: stepsMinusOne },
		};

		const migrated = migrateShellStore(blob, 7) as { onboarding: OnboardingState };
		expect(migrated.onboarding.steps.welcome.status).toBe('completed');
		// Missing step got filled in from defaults.
		expect(migrated.onboarding.steps.scaffolding.status).toBe('pending');
	});

	it('drops removed telemetry state during v15 migration', () => {
		const partial = createDefaultOnboardingState();
		(partial.steps as Record<string, any>).telemetry = { status: 'completed', completedAt: 123 };
		const blob = {
			activeMode: 'app',
			telemetryConsent: true,
			onboarding: { ...partial },
		};

		const migrated = migrateShellStore(blob, 14) as {
			onboarding: OnboardingState;
			telemetryConsent?: boolean;
		};
		expect(migrated.telemetryConsent).toBeUndefined();
		expect((migrated.onboarding.steps as Record<string, any>).telemetry).toBeUndefined();
	});

	it('still honours the v7 activeMode-snap behaviour', () => {
		const migrated = migrateShellStore(
			{
				activeMode: 'mail', // legacy mode no longer in the union
			},
			6
		) as { activeMode: string };
		// v16: the v7 snap now goes through normalizeMode.
		expect(migrated.activeMode).toBe('project');
	});

	it('maps a persisted `pkg:<id>` activeMode (v14) → project at v16', () => {
		const migrated = migrateShellStore(
			{
				activeMode: 'pkg:com.ikenga.tasks',
			},
			13
		) as { activeMode: string };
		expect(migrated.activeMode).toBe('project');
	});

	it('snaps a non-pkg unknown activeMode → project', () => {
		const migrated = migrateShellStore(
			{
				activeMode: 'studio', // dead mode, not a pkg: prefix
			},
			13
		) as { activeMode: string };
		expect(migrated.activeMode).toBe('project');
	});

	it('clamps a corrupt activeIndex to a valid range', () => {
		const partial = createDefaultOnboardingState();
		partial.activeIndex = 99;
		const migrated = migrateShellStore({ activeMode: 'app', onboarding: partial }, 7) as {
			onboarding: OnboardingState;
		};
		expect(migrated.onboarding.activeIndex).toBe(ONBOARDING_STEPS.length - 1);
	});
});

// ─── v16 / G-STATE ─────────────────────────────────────────────────────────
//
// Fixtures from plans/shell-ux-rearchitecture/drafts/g-state.md §4. Fixtures
// 1–4 and the rollback test go through the REAL zustand `persist` hydrate
// against jsdom's localStorage: each test seeds storage, then imports a fresh
// copy of the module (vi.resetModules) so `create(persist(...))` hydrates from
// exactly what was seeded.

type StoreModule = typeof import('./shell-store');

const KEY = 'shell-store';
const BACKUP_KEY = `${KEY}.__v15_backup`;

async function freshStore(): Promise<StoreModule> {
	vi.resetModules();
	return import('./shell-store');
}

function seedV15(state: Record<string, unknown>): string {
	const raw = JSON.stringify({ state, version: 15 });
	localStorage.setItem(KEY, raw);
	return raw;
}

/** A realistic v15 blob — every persisted v15 field (research §WP-02). */
function v15State(overrides: Record<string, unknown> = {}): Record<string, unknown> {
	return {
		activeMode: 'app',
		sidebarCollapsed: true,
		userName: 'Ada',
		defaultEngineId: 'com.ikenga.engine-claude-code',
		updatesAutoCheck: false,
		updatesAutoInstallApp: true,
		updatesAutoInstallPkgs: false,
		fileRoots: ['/home/ada/label'],
		claudeProjectRoots: [],
		claudeWatchEnabled: false,
		claudeBrowserMode: 'roots',
		onboarding: { ...createDefaultOnboardingState(), completedAt: 1_700_000_000_000 },
		...overrides,
	};
}

describe('shell-store v16 (G-STATE)', () => {
	beforeEach(() => {
		localStorage.clear();
		// The store logs expected Tauri-unavailable warnings in jsdom.
		vi.spyOn(console, 'warn').mockImplementation(() => {});
	});
	afterEach(() => {
		vi.restoreAllMocks();
		localStorage.clear();
	});

	it('fixture 1 — v15 with a package mode active → project, backup written, other fields kept', async () => {
		const state = v15State({ activeMode: 'pkg:com.ikenga.tasks' });
		const raw = seedV15(state);

		const { useShellStore } = await freshStore();
		const s = useShellStore.getState();

		expect(s.activeMode).toBe('project');
		// Backup = the exact incoming envelope.
		expect(localStorage.getItem(BACKUP_KEY)).toBe(raw);
		// The live key was rewritten as v16.
		expect(JSON.parse(localStorage.getItem(KEY)!).version).toBe(16);
		// Every other v15 field survives unchanged.
		expect(s.sidebarCollapsed).toBe(true);
		expect(s.userName).toBe('Ada');
		expect(s.defaultEngineId).toBe('com.ikenga.engine-claude-code');
		expect(s.updatesAutoCheck).toBe(false);
		expect(s.updatesAutoInstallApp).toBe(true);
		expect(s.updatesAutoInstallPkgs).toBe(false);
		expect(s.claudeWatchEnabled).toBe(false);
		expect(s.claudeBrowserMode).toBe('roots');
		expect(s.onboarding).toEqual(state.onboarding);
	});

	it('fixture 2 — v15 fileRoots + claudeProjectRoots survive deduplicated as activeProject.extra_roots', async () => {
		const fileRoots = ['/work/label', '  /work/shared  ', '/work/label'];
		const claudeProjectRoots = ['/work/shared', '/work/claude', '', '/work/claude '];
		seedV15(v15State({ fileRoots, claudeProjectRoots }));

		const { useShellStore } = await freshStore();
		const s = useShellStore.getState();

		const expected = ['/work/label', '/work/shared', '/work/claude'];
		expect(s.carriedRoots).toEqual(expected);
		expect(s.projectExtraRoots).toEqual({});
		expect(s.activeProject).toEqual({ id: 'default', root_path: null, extra_roots: expected });
		const stored = JSON.parse(localStorage.getItem(KEY)!).state;
		expect(stored.carriedRoots).toEqual(expected);
	});

	it('fixture 3 — fresh profile: migrate is NOT called, state equals the §2 defaults, no backup', async () => {
		expect(localStorage.length).toBe(0);
		const { useShellStore } = await freshStore();

		const assertDefaults = () => {
			const s = useShellStore.getState();
			expect(s.activeMode).toBe('project');
			expect(s.projectExtraRoots).toEqual({});
			expect(s.carriedRoots).toEqual([]);
			expect(s.activeProject).toEqual({ id: 'default', root_path: null, extra_roots: [] });
			expect(s.explorerSections).toEqual([
				{ id: 'files', source: 'shell', order: 0, collapsed: false },
				{ id: 'artifacts', source: 'shell', order: 1, collapsed: false },
				{ id: 'sessions', source: 'shell', order: 2, collapsed: false },
				{ id: 'ngwa-project', source: 'shell', order: 3, collapsed: true },
				{ id: 'automations', source: 'shell', order: 4, collapsed: false },
				{ id: 'todos', source: 'shell', order: 5, collapsed: true },
				{ id: 'scratchpads', source: 'shell', order: 6, collapsed: true },
				{ id: 'views', source: 'shell', order: 7, collapsed: true },
			]);
			expect(s.companion).toEqual({ activeTarget: { kind: 'new', engine_id: null } });
			// "everything else unchanged from v15" — spot-check the v15 initial values.
			expect(s.sidebarCollapsed).toBe(false);
			expect(s.onboarding).toEqual(createDefaultOnboardingState());
		};
		assertDefaults();

		// Re-run the real persist hydrate with a spy installed as `migrate`.
		const realMigrate = useShellStore.persist.getOptions().migrate!;
		const spy = vi.fn(realMigrate);
		useShellStore.persist.setOptions({ migrate: spy });
		await useShellStore.persist.rehydrate();

		expect(spy).not.toHaveBeenCalled();
		assertDefaults();
		expect(localStorage.getItem(BACKUP_KEY)).toBeNull();
	});

	it('fixture 3 control — the same spy IS called when a v15 blob is stored', async () => {
		const { useShellStore } = await freshStore();
		const spy = vi.fn(useShellStore.persist.getOptions().migrate!);
		useShellStore.persist.setOptions({ migrate: spy });
		seedV15(v15State());
		await useShellStore.persist.rehydrate();
		expect(spy).toHaveBeenCalledTimes(1);
		expect(spy.mock.calls[0]![1]).toBe(15);
	});

	it('fixture 4 — corrupt/partial v15 blob loads without throwing', async () => {
		seedV15({ activeMode: 42, fileRoots: 'x' });

		const { useShellStore } = await freshStore();
		const s = useShellStore.getState();

		expect(s.activeMode).toBe('project');
		expect(s.carriedRoots).toEqual([]);
		expect(s.activeProject.extra_roots).toEqual([]);
		expect(s.onboarding).toEqual(createDefaultOnboardingState());
		expect(s.explorerSections).toEqual(createDefaultExplorerSections());
		expect(localStorage.getItem(BACKUP_KEY)).not.toBeNull();
	});

	it('fixture 4b — a non-JSON shell-store value falls back to defaults without throwing', async () => {
		localStorage.setItem(KEY, '{not json');
		const { useShellStore } = await freshStore();
		expect(useShellStore.getState().activeMode).toBe('project');
		expect(useShellStore.getState().carriedRoots).toEqual([]);
	});

	it('rollback — restoreV15Backup puts back the byte-equal v15 blob and removes the backup', async () => {
		const raw = seedV15(
			v15State({
				activeMode: 'artifact-grid',
				fileRoots: ['/work/label', '/work/shared'],
				claudeProjectRoots: ['/work/shared'],
			})
		);

		const { useShellStore, restoreV15Backup } = await freshStore();
		// v16 is live and the key holds a v16 blob.
		expect(useShellStore.getState().activeMode).toBe('project');
		expect(localStorage.getItem(KEY)).not.toBe(raw);
		expect(JSON.parse(localStorage.getItem(KEY)!).version).toBe(16);

		expect(restoreV15Backup()).toBe(true);

		expect(localStorage.getItem(KEY)).toBe(raw);
		expect(localStorage.getItem(BACKUP_KEY)).toBeNull();
		// Second call is a no-op.
		expect(restoreV15Backup()).toBe(false);
		expect(localStorage.getItem(KEY)).toBe(raw);
	});

	it('does not overwrite an existing __v15_backup on a later migration', async () => {
		localStorage.setItem(BACKUP_KEY, 'earlier-backup');
		seedV15(v15State());
		await freshStore();
		expect(localStorage.getItem(BACKUP_KEY)).toBe('earlier-backup');
	});

	it('a v16 blob rehydrates without migrate and re-derives activeProject', async () => {
		localStorage.setItem(
			KEY,
			JSON.stringify({
				state: {
					activeMode: 'chi',
					carriedRoots: ['/a'],
					projectExtraRoots: { default: ['/b', '/a'] },
					// Never read from a blob:
					activeProject: { id: 'x', root_path: '/x', extra_roots: [] },
					companion: { activeTarget: { kind: 'session', session_id: 'dead' } },
				},
				version: 16,
			})
		);
		const { useShellStore } = await freshStore();
		const s = useShellStore.getState();
		expect(s.activeMode).toBe('chi');
		expect(s.activeProject).toEqual({ id: 'default', root_path: null, extra_roots: ['/b', '/a'] });
		expect(s.companion).toEqual({ activeTarget: { kind: 'new', engine_id: null } });
		expect(localStorage.getItem(BACKUP_KEY)).toBeNull();
	});

	it('setActiveMode stores the four modes and can never store anything else', async () => {
		const { useShellStore } = await freshStore();
		const { setActiveMode } = useShellStore.getState();
		for (const m of ACTIVITY_MODES) {
			setActiveMode(m);
			expect(useShellStore.getState().activeMode).toBe(m);
		}
		// WP-03 removed the legacy input arm from the type. An untyped caller
		// (a bridge payload cast to CoreMode) still cannot store a pre-v16 name.
		setActiveMode('files' as CoreMode);
		expect(useShellStore.getState().activeMode).toBe('project');
		setActiveMode('pkg:com.ikenga.tasks' as CoreMode);
		expect(useShellStore.getState().activeMode).toBe('project');
		setActiveMode('chi');
		expect(JSON.parse(localStorage.getItem(KEY)!).state.activeMode).toBe('chi');
	});

	it('isPreV16ModeName recognises exactly the names /iyke/mode still normalizes', async () => {
		const { isPreV16ModeName } = await freshStore();
		for (const m of ['app', 'files', 'sessions', 'artifact-grid', 'pkgs', 'pkg:com.ikenga.tasks']) {
			expect(isPreV16ModeName(m)).toBe(true);
		}
		for (const m of [...ACTIVITY_MODES, 'mail', '', 42, null]) {
			expect(isPreV16ModeName(m)).toBe(false);
		}
	});

	it('setProjectExtraRoots trims, dedupes and recomputes activeProject', async () => {
		seedV15(v15State({ fileRoots: ['/carried'] }));
		const { useShellStore } = await freshStore();
		const before = useShellStore.getState().activeProject;
		useShellStore.getState().setProjectExtraRoots('default', [' /mine ', '/mine', '/carried', '']);
		const s = useShellStore.getState();
		expect(s.projectExtraRoots).toEqual({ default: ['/mine', '/carried'] });
		expect(s.activeProject.extra_roots).toEqual(['/mine', '/carried']);
		expect(s.activeProject).not.toBe(before);
		// Roots for another project don't touch the active one — same reference.
		const current = s.activeProject;
		useShellStore.getState().setProjectExtraRoots('other', ['/elsewhere']);
		expect(useShellStore.getState().activeProject).toBe(current);
	});

	it('explorer sections: collapse + move keep integer orders 0..n-1', async () => {
		const { useShellStore } = await freshStore();
		const st = () => useShellStore.getState();
		st().setExplorerSectionCollapsed('todos', false);
		expect(st().explorerSections.find((x) => x.id === 'todos')!.collapsed).toBe(false);

		st().moveExplorerSection('artifacts', -1);
		expect(
			st()
				.explorerSections.map((x) => x.id)
				.slice(0, 2)
		).toEqual(['artifacts', 'files']);
		expect(st().explorerSections.map((x) => x.order)).toEqual([0, 1, 2, 3, 4, 5, 6, 7]);

		// Moving past either end is a no-op.
		const snapshot = st().explorerSections;
		st().moveExplorerSection('artifacts', -1);
		st().moveExplorerSection('views', 1);
		expect(st().explorerSections).toBe(snapshot);
	});

	it('companion target is set in memory but never persisted', async () => {
		const { useShellStore } = await freshStore();
		useShellStore.getState().setCompanionTarget({ kind: 'session', session_id: 'abc' });
		expect(useShellStore.getState().companion.activeTarget).toEqual({
			kind: 'session',
			session_id: 'abc',
		});
		const stored = JSON.parse(localStorage.getItem(KEY)!).state;
		expect(stored.companion).toBeUndefined();
		expect(stored.activeProject).toBeUndefined();
		expect(stored.projects).toBeUndefined();
		expect(stored.activeProjectId).toBeUndefined();
		expect(stored.explorerSections).toHaveLength(8);
		expect(stored.projectExtraRoots).toEqual({});
		expect(stored.carriedRoots).toEqual([]);
	});
});

describe('normalizeMode / dedupeRoots', () => {
	it('maps every v15 mode per g-state.md §4 and is total', () => {
		expect(ACTIVITY_MODES).toEqual(['project', 'chi', 'ngwa', 'settings']);
		for (const m of ['app', 'files', 'sessions', 'artifact-grid', 'pkg:com.ikenga.tasks']) {
			expect(normalizeMode(m)).toBe('project');
		}
		expect(normalizeMode('pkgs')).toBe('ngwa');
		expect(normalizeMode('ngwa')).toBe('ngwa');
		expect(normalizeMode('settings')).toBe('settings');
		expect(normalizeMode('project')).toBe('project');
		expect(normalizeMode('chi')).toBe('chi');
		for (const m of [undefined, null, 42, {}, [], '', 'mail', 'PROJECT']) {
			expect(normalizeMode(m)).toBe('project');
		}
	});

	it('dedupeRoots trims, drops non-strings/empties, keeps first occurrence', () => {
		expect(dedupeRoots(['/a', ' /a ', 3, null, '', '  ', '/b', '/a'])).toEqual(['/a', '/b']);
		expect(dedupeRoots('x')).toEqual([]);
		expect(dedupeRoots(undefined)).toEqual([]);
	});

	it('migrateShellStore v16 step: roots union, legacy fields untouched, explorer defaults', () => {
		const migrated = migrateShellStore(
			{ activeMode: 'pkgs', fileRoots: ['/a', 7, ' /b'], claudeProjectRoots: ['/b', '/c'] },
			15
		) as Record<string, unknown>;
		expect(migrated.activeMode).toBe('ngwa');
		expect(migrated.carriedRoots).toEqual(['/a', '/b', '/c']);
		expect(migrated.projectExtraRoots).toEqual({});
		expect(migrated.fileRoots).toEqual(['/a', 7, ' /b']);
		expect(migrated.explorerSections).toEqual(createDefaultExplorerSections());
	});
});
