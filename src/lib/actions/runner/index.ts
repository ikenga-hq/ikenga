// WP-53 — the action runner: `runAction(action, ctx)` over the six run
// kinds of a personal / project action (G-ACTIONS §8.1), with the six
// variables (§8.2), `confirm`, Test run, and the DEC-55 project-trust gate
// (§8.3). WP-55 (menus) and WP-58 (Test run) call this; package actions
// (`dispatch` fill-only, `view`) and built-ins never come through here.
//
// Every run ends in one typed outcome — never a throw:
//   done     it executed (`chi` / `skill`: the run id, DEC-63.3)
//   failed   it executed and the host reported a failure
//   refused  it did not execute, with a typed reason: the trust gate
//            (`untrusted` / `changed` / `trust-unavailable`, plus the
//            `project-actions` trust-sheet request to offer), `workflow`'s
//            `no-workflow-runner`, `cancelled` (confirm), bad input, …
//   preview  Test run of a `shell` action: the interpolated command, NOT run
//
// Test run never writes `actions.json` / `keybindings.json` (the runner
// writes neither in any mode) and never executes `shell`; `chi` still sends
// and shows the run id (D-06 `testRun`).

import { getContextKeys } from '@/lib/keymap/context-keys';
import { findLeaf } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useShellStore } from '@/lib/shell/shell-store';
import type { ActionRun, ActionRunKind, ActionsScope, RunVariable, UserAction } from '../types';
import { ChiUnavailableError, ptySafeVariables, send as chiSend, type ChiSendResult } from './chi';
import {
	basename,
	emptyRunVariables,
	interpolate,
	mentionsVariable,
	UnknownVariableError,
	type RunVariables,
} from './interpolate';
import { callIyke, iykeRequest, type IykeCallResult } from './iyke';
import { classifyOpenUrl, openTarget, type OpenTarget } from './open';
import { actionExec, actionGitBranch, previewShellRun, type ActionExecResult } from './shell';
import { isValidSkillName, runSkill } from './skill';
import { checkActionTrust, runHash, type TrustRefusal } from './trust';
import { workflowDisabled } from './workflow';

export { canonicalJson, checkActionTrust, isTrustGated, onTrustChanged, runHash } from './trust';
export { interpolate, templateVariables } from './interpolate';
export type { ActionExecRefusal, ActionExecResult } from './shell';
export type { ChiSendResult } from './chi';

// --- inputs ----------------------------------------------------------------------

/** What the runner needs of an action. A `UserAction` satisfies it. */
export interface RunnableAction {
	id: string;
	name?: string;
	run: ActionRun;
	/** Where the action is defined — decides the trust gate. */
	scope: ActionsScope;
}

export interface ConfirmRequest {
	actionId: string;
	name: string;
	/** The command with its values filled in, for reading. (At run time each
	 *  value is an environment variable — never spliced into the text.) */
	command: string;
	/** Null = the home directory. */
	cwd: string | null;
}

export interface RunContext {
	/** The project whose trust record gates a project action; default: the active one. */
	projectId?: string | null;
	/**
	 * Variable values from the menu context (the row / tab the menu opened
	 * on, §1.3). A value given here wins; missing ones are read live.
	 */
	variables?: Partial<Record<RunVariable, string | null | undefined>>;
	/** WP-58 Test run: never executes `shell`. */
	testRun?: boolean;
	/** `shell` `confirm: true`. Default: `window.confirm`. */
	confirm?: (request: ConfirmRequest) => boolean | Promise<boolean>;
}

// --- outcomes --------------------------------------------------------------------

export type RunRefusalReason =
	| TrustRefusal
	| 'no-workflow-runner'
	| 'no-engine'
	| 'no-target'
	/** A PTY inject whose text starts a line with `!` (Claude Code bash mode). */
	| 'bang-prompt'
	/** Windows: a value the command names holds a `cmd.exe` metacharacter. */
	| 'unsafe-value-for-windows'
	| 'invalid-skill'
	| 'invalid-route'
	| 'invalid-url'
	| 'unknown-variable'
	| 'unknown-kind'
	| 'variable-in-single-quotes'
	| 'variable-after-escape'
	/** `action_exec` refused for another reason (not found, bad cwd, …). */
	| 'exec-refused'
	| 'cancelled';

/** What to open when a run is refused by the trust gate: WP-18's sheet in
 *  its `project-actions` mode, focused on this action. */
export interface TrustSheetRequest {
	mode: 'project-actions';
	projectId: string | null;
	actionIds: string[];
}

export type RunOutcome =
	| {
			status: 'done';
			kind: ActionRunKind;
			testRun: boolean;
			/** `chi` / `skill`: the Chi run id (`null` when injected into a live PTY). */
			runId?: string | null;
			via?: ChiSendResult['via'];
			exec?: ActionExecResult;
			iyke?: IykeCallResult;
			opened?: Exclude<OpenTarget, { kind: 'refused' }>;
	  }
	| {
			status: 'failed';
			kind: ActionRunKind;
			testRun: boolean;
			message: string;
			exec?: ActionExecResult;
			iyke?: IykeCallResult;
	  }
	| {
			status: 'refused';
			kind: ActionRunKind | string;
			reason: RunRefusalReason;
			message: string;
			trustSheet?: TrustSheetRequest;
	  }
	| {
			status: 'preview';
			kind: 'shell';
			testRun: true;
			command: string;
			cwd: string | null;
			confirm: boolean;
	  };

function refused(
	kind: ActionRunKind | string,
	reason: RunRefusalReason,
	message: string,
	trustSheet?: TrustSheetRequest
): RunOutcome {
	return trustSheet
		? { status: 'refused', kind, reason, message, trustSheet }
		: { status: 'refused', kind, reason, message };
}

function errorText(err: unknown): string {
	return err instanceof Error ? err.message : String(err);
}

// --- variables -------------------------------------------------------------------

function focusedPaneUrl(): string {
	const { root, focusedId } = usePaneStore.getState();
	const leaf = findLeaf(root, focusedId);
	const view = leaf?.tabs[leaf.activeTabIdx];
	if (!view) return '';
	switch (view.kind) {
		case 'route':
		case 'artifact':
		case 'artifact-studio':
			return view.path;
		default:
			return '';
	}
}

function liveSelection(): string {
	try {
		return typeof window === 'undefined' ? '' : (window.getSelection()?.toString() ?? '');
	} catch {
		return '';
	}
}

function liveResource(): string {
	try {
		return getContextKeys().resource ?? '';
	} catch {
		return '';
	}
}

/** Every template of a run (what may name a variable). */
function runTemplates(run: ActionRun): string[] {
	switch (run.kind) {
		case 'chi':
			return [run.prompt];
		case 'shell':
			return [run.command, run.cwd ?? '{{project.root}}'];
		case 'open':
			return [run.url];
		default:
			return [];
	}
}

/**
 * The six values (§8.2) for a run. Explicit `overrides` win; the rest are
 * read live. `branch` (a Rust read of `.git/HEAD`) is only looked up when
 * a template names it or the kind ships all six (`iyke`).
 */
export async function gatherRunVariables(
	run: ActionRun,
	overrides: RunContext['variables'] = {}
): Promise<RunVariables> {
	const values = emptyRunVariables();
	const given = (name: RunVariable): string | undefined => {
		const value = overrides?.[name];
		return value === undefined ? undefined : (value ?? '');
	};
	const all = run.kind === 'iyke';
	const templates = runTemplates(run);
	const needs = (name: RunVariable) => all || mentionsVariable(name, ...templates);

	values['project.root'] = given('project.root') ?? useShellStore.getState().activeProject.root_path ?? '';
	values['file.path'] = given('file.path') ?? liveResource();
	values['file.name'] = given('file.name') ?? (values['file.path'] ? basename(values['file.path']) : '');
	values.selection = given('selection') ?? (needs('selection') ? liveSelection() : '');
	values['pane.url'] = given('pane.url') ?? focusedPaneUrl();
	const branch = given('branch');
	if (branch !== undefined) values.branch = branch;
	else if (needs('branch') && values['project.root']) {
		try {
			values.branch = (await actionGitBranch(values['project.root'])) ?? '';
		} catch {
			values.branch = '';
		}
	}
	return values;
}

// --- the runner ------------------------------------------------------------------

const RUN_KINDS: readonly ActionRunKind[] = ['chi', 'shell', 'iyke', 'skill', 'workflow', 'open'];

function defaultConfirm(request: ConfirmRequest): boolean {
	if (typeof window === 'undefined' || typeof window.confirm !== 'function') return false;
	return window.confirm(
		`Run “${request.name}”?\n\n${request.command}\n\nin ${request.cwd ?? 'your home directory'}`
	);
}

/** Maps `action_exec`'s typed refusal onto a run outcome. */
function execRefusal(exec: ActionExecResult, projectId: string | null, actionId: string): RunOutcome {
	const message = exec.error ?? 'The command was refused.';
	switch (exec.refusal) {
		case 'untrusted':
		case 'changed':
		case 'trust-unavailable':
			return refused('shell', exec.refusal, message, {
				mode: 'project-actions',
				projectId,
				actionIds: [actionId],
			});
		case 'variable-in-single-quotes':
		case 'variable-after-escape':
		case 'unknown-variable':
		case 'unsafe-value-for-windows':
			return refused('shell', exec.refusal, message);
		default:
			return refused('shell', 'exec-refused', message);
	}
}

function chiRefusal(kind: ActionRunKind, testRun: boolean, err: unknown): RunOutcome {
	if (err instanceof ChiUnavailableError) return refused(kind, err.reason, err.message);
	return { status: 'failed', kind, testRun, message: errorText(err) };
}

/**
 * Runs one personal or project action. Never throws: every path ends in a
 * `RunOutcome`.
 */
export async function runAction(action: RunnableAction | UserAction, ctx: RunContext = {}): Promise<RunOutcome> {
	const run = action.run as ActionRun | undefined;
	const kind = (run as { kind?: unknown } | undefined)?.kind;
	if (!run || typeof kind !== 'string' || !RUN_KINDS.includes(kind as ActionRunKind)) {
		return refused(String(kind ?? 'unknown'), 'unknown-kind', `“${action.id}” has no runnable kind.`);
	}
	const testRun = ctx.testRun === true;
	const name = typeof action.name === 'string' && action.name ? action.name : action.id;
	const projectId =
		ctx.projectId !== undefined ? ctx.projectId : (useShellStore.getState().activeProject.id ?? null);

	// A Test run of `shell` shows the command and stops before the gate:
	// nothing executes, so there is nothing to trust yet.
	const previewOnly = testRun && run.kind === 'shell';

	// DEC-55 gate — fail-closed, against the in-force trust status.
	let gateHash: string | null = null;
	if (!previewOnly) {
		const trust = await checkActionTrust({ id: action.id, run, scope: action.scope }, projectId);
		if (!trust.ok) {
			return refused(run.kind, trust.reason, trust.message, {
				mode: 'project-actions',
				projectId: projectId ?? null,
				actionIds: [action.id],
			});
		}
		gateHash = trust.hash;
	}

	let variables: RunVariables;
	try {
		variables = await gatherRunVariables(run, ctx.variables);
	} catch (err) {
		return { status: 'failed', kind: run.kind, testRun, message: errorText(err) };
	}

	try {
		switch (run.kind) {
			case 'workflow': {
				const disabled = workflowDisabled(run);
				return refused(run.kind, disabled.reason, disabled.message);
			}

			case 'chi': {
				const prompt = interpolate(run.prompt, variables, 'raw');
				// What a PTY inject types: control characters stripped from each
				// value before interpolation (`chi.ts` module note).
				const ptyPrompt = interpolate(run.prompt, ptySafeVariables(variables), 'raw');
				try {
					const sent = await chiSend({
						prompt,
						ptyPrompt,
						target: run.target,
						...(run.engineId ? { engineId: run.engineId } : {}),
						scope: action.scope,
					});
					return { status: 'done', kind: run.kind, testRun, runId: sent.runId, via: sent.via };
				} catch (err) {
					return chiRefusal(run.kind, testRun, err);
				}
			}

			case 'skill': {
				if (!isValidSkillName(run.skill.trim())) {
					return refused(run.kind, 'invalid-skill', `“${run.skill}” is not a valid skill name.`);
				}
				try {
					const sent = await runSkill(run, action.scope);
					return { status: 'done', kind: run.kind, testRun, runId: sent.runId, via: sent.via };
				} catch (err) {
					return chiRefusal(run.kind, testRun, err);
				}
			}

			case 'shell': {
				const prepared = previewShellRun(run, variables);
				if (testRun) {
					return {
						status: 'preview',
						kind: 'shell',
						testRun: true,
						command: prepared.command,
						cwd: prepared.cwd,
						confirm: prepared.confirm,
					};
				}
				if (prepared.confirm) {
					const ask = ctx.confirm ?? defaultConfirm;
					const yes = await ask({ actionId: action.id, name, command: prepared.command, cwd: prepared.cwd });
					if (!yes) return refused(run.kind, 'cancelled', 'Cancelled.');
				}
				// No command text crosses: Rust loads the pinned `run` for this
				// id, re-checks the hash (and, for a project action, its trust)
				// and passes the values as environment variables (§8.2).
				const exec = await actionExec({
					scope: action.scope,
					projectId: projectId ?? null,
					actionId: action.id,
					runHash: gateHash ?? (await runHash(run)),
					variables,
				});
				if (exec.refusal) return execRefusal(exec, projectId ?? null, action.id);
				if (exec.ok) return { status: 'done', kind: run.kind, testRun, exec };
				const message =
					exec.error ??
					(exec.exitCode !== null ? `Exited with status ${exec.exitCode}.` : 'The command failed.');
				return { status: 'failed', kind: run.kind, testRun, message, exec };
			}

			case 'iyke': {
				const request = iykeRequest(run, variables);
				if (!request) {
					return refused(run.kind, 'invalid-route', `“${run.route}” is not an iyke bridge route.`);
				}
				const iyke = await callIyke(request.path, request.init);
				if (iyke.ok) return { status: 'done', kind: run.kind, testRun, iyke };
				return {
					status: 'failed',
					kind: run.kind,
					testRun,
					message: `iyke ${request.path} answered ${iyke.status}.`,
					iyke,
				};
			}

			case 'open': {
				const target = classifyOpenUrl(interpolate(run.url, variables, 'uri'));
				if (target.kind === 'refused') return refused(run.kind, 'invalid-url', target.message);
				await openTarget(target);
				return { status: 'done', kind: run.kind, testRun, opened: target };
			}
		}
	} catch (err) {
		if (err instanceof UnknownVariableError) return refused(run.kind, 'unknown-variable', err.message);
		return { status: 'failed', kind: run.kind, testRun, message: errorText(err) };
	}
}
