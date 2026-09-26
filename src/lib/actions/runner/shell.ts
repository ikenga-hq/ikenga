// WP-53 — the `shell` run kind (G-ACTIONS §8.1): interpolate the command
// with every value quoted as one argument for the executing shell (§8.2),
// resolve `cwd` (default `{{project.root}}`, else the home directory), and
// run it headless through `action_exec` (`src-tauri/src/commands/action_exec.rs`).
//
// `action_exec` runs `/bin/sh -c` on macOS / Linux and PowerShell on
// Windows; `shellFlavor()` picks the matching quoting. Test run (WP-58)
// NEVER executes: it returns the interpolated command for display.

import { invoke } from '@/lib/tauri-cmd';
import type { ActionRun, ActionsScope } from '../types';
import { interpolate, type InterpolationMode, type RunVariables } from './interpolate';

export type ShellRun = Extract<ActionRun, { kind: 'shell' }>;

export interface ActionExecRequest {
	command: string;
	cwd: string | null;
	scope: ActionsScope;
	projectId: string | null;
	/** Required for a project action: Rust re-checks its trust pin. */
	actionId: string | null;
	runHash: string | null;
	timeoutSecs?: number | null;
}

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
}

export type ShellFlavor = 'posix' | 'powershell';

/** The shell `action_exec` runs on this platform. */
export function shellFlavor(
	platform: string = typeof navigator === 'undefined' ? '' : navigator.platform || navigator.userAgent
): ShellFlavor {
	return /win/i.test(platform) && !/darwin|mac/i.test(platform) ? 'powershell' : 'posix';
}

function modeFor(flavor: ShellFlavor): InterpolationMode {
	return flavor === 'powershell' ? 'shell-powershell' : 'shell-posix';
}

export interface PreparedShellRun {
	/** The command exactly as it will run. */
	command: string;
	/** Absolute cwd, or null = the home directory (resolved in Rust). */
	cwd: string | null;
	confirm: boolean;
}

/**
 * Interpolates `command` (quoted) and `cwd`. `cwd` is a path, not a shell
 * word, so its values are spliced raw: it never reaches a shell, only
 * `current_dir`. Throws `UnknownVariableError`.
 */
export function prepareShellRun(
	run: ShellRun,
	variables: RunVariables,
	flavor: ShellFlavor = shellFlavor()
): PreparedShellRun {
	const command = interpolate(run.command, variables, modeFor(flavor));
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
