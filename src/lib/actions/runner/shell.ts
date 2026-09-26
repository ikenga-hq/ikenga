// WP-53 — the `shell` run kind (G-ACTIONS §8.1), run headless through
// `action_exec` (`src-tauri/src/commands/action_exec.rs`).
//
// The frontend never builds shell text. It sends the action's identity —
// `scope`, `actionId`, the canonical-`run` hash it gated on — and the six
// variable values. Rust loads the pinned `run` from the in-force document
// (a project action must be trusted at that hash; a personal one must exist
// in the personal file at that hash), passes every value as an environment
// variable (`IKENGA_FILE_PATH`, …) and rewrites each `{{var}}` to the host
// shell's reference to it, so a value is never re-parsed (§8.2). Rust also
// owns the shell (`/bin/sh` on unix, PowerShell `-EncodedCommand` on
// Windows), so there is no quoting flavour to keep in sync here.
//
// `previewShellRun` substitutes values raw, for DISPLAY only (the confirm
// prompt and the WP-58 Test-run preview, which never executes).

import { invoke } from '@/lib/tauri-cmd';
import type { ActionRun, ActionsScope } from '../types';
import { interpolate, type RunVariables } from './interpolate';

export type ShellRun = Extract<ActionRun, { kind: 'shell' }>;

export interface ActionExecRequest {
	scope: ActionsScope;
	projectId: string | null;
	actionId: string;
	/** SHA-256 of the canonical `run` JSON the gate checked (B-14). */
	runHash: string;
	/** The six values by variable name; they become environment variables. */
	variables: RunVariables;
	timeoutSecs?: number | null;
}

/** Why `action_exec` spawned nothing (see the Rust module note). */
export type ActionExecRefusal =
	| 'untrusted'
	| 'changed'
	| 'trust-unavailable'
	| 'unavailable'
	| 'not-found'
	| 'not-shell'
	| 'unknown-scope'
	| 'unknown-variable'
	| 'variable-in-single-quotes'
	| 'variable-after-escape'
	| 'invalid-variable'
	| 'unsafe-value-for-windows'
	| 'invalid-cwd';

export interface ActionExecResult {
	ok: boolean;
	exitCode: number | null;
	stdout: string;
	stderr: string;
	stdoutTruncated: boolean;
	stderrTruncated: boolean;
	timedOut: boolean;
	cwd: string;
	shell: 'sh' | 'powershell' | '';
	error: string | null;
	refusal: ActionExecRefusal | null;
}

export interface PreparedShellRun {
	/** The command with values substituted raw — for display, never run. */
	command: string;
	/** Absolute cwd, or null = the home directory (resolved in Rust). */
	cwd: string | null;
	confirm: boolean;
}

/**
 * What a `shell` run will do, for the confirm prompt and Test-run preview.
 * Throws `UnknownVariableError`. The executed command is built in Rust.
 */
export function previewShellRun(run: ShellRun, variables: RunVariables): PreparedShellRun {
	const command = interpolate(run.command, variables, 'raw');
	const cwdTemplate = run.cwd ?? '{{project.root}}';
	const cwd = interpolate(cwdTemplate, variables, 'raw').trim();
	return { command, cwd: cwd === '' ? null : cwd, confirm: run.confirm === true };
}

export function actionExec(request: ActionExecRequest): Promise<ActionExecResult> {
	return invoke<ActionExecResult>('action_exec', { request });
}

/** `{{branch}}`: the git branch checked out at `root`, or null. */
export function actionGitBranch(root: string): Promise<string | null> {
	return invoke<string | null>('action_git_branch', { root });
}
