// WP-53 — the narrow Chi adapter the `chi` and `skill` run kinds code
// against (09-orchestration Mock contract 3):
//
//   send({ prompt, target })               → Promise<{ runId }>
//   invokeSkill({ skill, args, target })   → Promise<{ runId }>
//
// It sits on today's Companion dispatch path: `resolveTarget` decides WHERE
// the text goes (live PTY inject / `chi_resume` / `chi_run`) exactly as the
// dispatch bar does, and this adapter then makes the same host call — but
// keeps the result, because an action run (unlike the dispatch bar, ADR-021)
// returns its run id (DEC-63.3: "Dispatch to Chi" SENDS and returns a run
// id). `resolve-target.ts` internals are not touched; if #248
// (`feat/chi-openrouter-in-process`) lands first, only the bodies below
// rebind to its `chi_run` shape.
//
// A PTY inject has no Chi run behind it, so `runId` is `null` there
// (`via: 'pty'`). A PTY inject is a keystroke stream into a TUI — Claude
// Code's `!` bash mode runs a leading `!` line as a shell command with no
// permission prompt, and a wrap tab whose agent has exited is a plain shell
// (`claude-wrap.ts` falls back to `exec "${SHELL:-bash}" -i`) — so it is
// held to these rules (DEC-55):
//
// - Only a PERSONAL action may inject. A project or package (or any
//   non-personal) `chi` / `skill` runs headless only (`chi_run` /
//   `chi_resume`); an `active` target that resolves to a PTY is unavailable
//   to it (`no-target`).
// - Only into a CLAUDE agent terminal (a `wrap` spec with engine `claude`),
//   never a plain shell, and only while the store says its agent is live —
//   fail-closed (`isAgentLive`): a `SessionStart` id is recorded, no
//   `SessionEnd` / PTY exit since, PTY running. Other engines' wraps send no
//   liveness signal, so they are never injected into (`no-target`).
// - Not while that terminal has a pending permission request: the inject's
//   trailing Enter would answer it (`permission-pending`).
// - The injected text comes with every C0 control character (bar tab)
//   stripped from each variable value before interpolation (`ptyPrompt`,
//   built by the runner with `ptySafeVariables`), and a text whose first
//   non-space character is `!`, or with a line starting (after spaces) with
//   `!`, is refused (`bang-prompt`).
//
// The manifest `dispatch` kind and built-in "Hand to Chi" stay fill-only
// and never come through here.

import { chiResume, chiRun, type ChiRunResult } from '@/lib/tauri-cmd';
import { useShellStore, type CompanionTarget } from '@/lib/shell/shell-store';
import { useCompanionStore } from '@/shell/companion/companion-store';
import { resolveTarget } from '@/shell/companion/resolve-target';
import { useTerminalStore } from '@/terminal/session-store';
import type { ActionsScope, ChiTarget } from '../types';
import type { RunVariables } from './interpolate';

/** Where a runnable action comes from: a personal / project actions file,
 *  or a package's skill actions (package content — never DEC-55-gated,
 *  never PTY-injected). */
export type RunScope = ActionsScope | 'package';

export interface ChiSendRequest {
	/** Final text (already interpolated, raw — §8.2); what a headless run gets. */
	prompt: string;
	/** The same template interpolated with `ptySafeVariables` — what a PTY
	 *  inject types. Default: `prompt` (a text with no variables). */
	ptyPrompt?: string;
	target: ChiTarget;
	/** Required iff `target === 'engine'`. */
	engineId?: string;
	/** Where the action is defined. Only `personal` may inject into a PTY. */
	scope: RunScope;
}

export interface ChiSkillRequest {
	skill: string;
	/** Free text after the skill invocation; optional. */
	args?: string;
	target: ChiTarget;
	engineId?: string;
	scope: RunScope;
}

export interface ChiSendResult {
	/** The Chi run id; `null` when the text was injected into a live PTY. */
	runId: string | null;
	via: 'chi-run' | 'chi-resume' | 'pty';
}

/** Nothing to send to — the typed reason the runner reports. */
export type ChiUnavailableReason = 'no-engine' | 'no-target' | 'bang-prompt' | 'permission-pending';

export class ChiUnavailableError extends Error {
	readonly reason: ChiUnavailableReason;

	constructor(reason: ChiUnavailableReason, message: string) {
		super(message);
		this.name = 'ChiUnavailableError';
		this.reason = reason;
	}
}

/** Maps an action's `target` onto the Companion's target shape. */
export function companionTargetFor(target: ChiTarget, engineId?: string): CompanionTarget {
	switch (target) {
		case 'active':
			return useShellStore.getState().companion.activeTarget;
		case 'new':
			return { kind: 'new', engine_id: null };
		case 'engine':
			if (!engineId) throw new ChiUnavailableError('no-target', 'A `chi` run with target "engine" needs an engineId');
			return { kind: 'new', engine_id: engineId };
	}
}

/** Whether terminal `sessionId` runs an agent TUI (a `wrap` spec) rather
 *  than a shell. Mirrors `resolve-target.ts`'s (unexported) check. */
export function isAgentTerminal(sessionId: string): boolean {
	const tab = useTerminalStore.getState().tabs.find((t) => t.id === sessionId);
	return Boolean(tab?.spec.wrap);
}

/**
 * Whether the Claude agent in wrap tab `sessionId` is live — fail-closed.
 * True only when the tab is a Claude wrap, its `SessionStart` id is
 * recorded (`typeof claudeSessionId === 'string'`), the store marks the
 * agent live (set on `SessionStart`, cleared on `SessionEnd`, PTY exit and
 * restart; never restored from disk) and the PTY is running. A never-started
 * agent (`undefined` id), a stale id after an exit, or a restored tab all
 * read not-live.
 */
export function isAgentLive(sessionId: string): boolean {
	const tab = useTerminalStore.getState().tabs.find((t) => t.id === sessionId);
	const wrap = tab?.spec.wrap;
	if (!tab || !wrap || (wrap.engine ?? 'claude') !== 'claude') return false;
	return typeof tab.claudeSessionId === 'string' && tab.agentLive === true && tab.status === 'running';
}

/** Whether wrap tab `sessionId` runs an engine other than Claude — no
 *  liveness signal, so never injectable. */
export function isNonClaudeAgent(sessionId: string): boolean {
	const wrap = useTerminalStore.getState().tabs.find((t) => t.id === sessionId)?.spec.wrap;
	return Boolean(wrap) && (wrap?.engine ?? 'claude') !== 'claude';
}

/** Whether terminal `sessionId` has a pending permission request in the
 *  Companion's queue (fed from the hooks bus). A pending card with no
 *  terminal id could be this one, so it counts too (fail-closed). */
export function hasPendingPermission(sessionId: string): boolean {
	return useCompanionStore
		.getState()
		.permissions.some((p) => p.status === 'pending' && (p.sessionId === undefined || p.sessionId === sessionId));
}

/** C0 controls except tab (CR and LF included), plus DEL and C1. */
function isPtyControl(code: number): boolean {
	return (code < 0x20 && code !== 0x09) || (code >= 0x7f && code <= 0x9f);
}

/** A value made safe to type into a TUI (what a PTY inject strips from each
 *  variable value): a run of line breaks becomes one space, every other
 *  control character is dropped. */
export function stripPtyControls(value: string): string {
	let out = '';
	for (const ch of value.replace(/[\r\n]+/g, ' ')) {
		if (!isPtyControl(ch.codePointAt(0) ?? 0)) out += ch;
	}
	return out;
}

/** Every value through `stripPtyControls` — the set a `ptyPrompt` is
 *  interpolated with (before interpolation, so the template's own line
 *  breaks survive and are still checked by `isBangPrompt`). */
export function ptySafeVariables(values: RunVariables): RunVariables {
	const out = { ...values };
	for (const name of Object.keys(out) as (keyof RunVariables)[]) out[name] = stripPtyControls(out[name]);
	return out;
}

/** Claude Code bash mode: a line whose first non-space character is `!`. */
const BANG_RE = /(^\s*|[\r\n][ \t]*)!/;

export function isBangPrompt(text: string): boolean {
	return BANG_RE.test(text);
}

export const PTY_SCOPE_REASON =
	'Only a personal action types into a terminal — a project or package action runs headless. Set its target to "new" or "engine", or pick a headless Companion target.';

export const AGENT_NOT_LIVE_REASON =
	'The agent in that terminal is not running (not started yet, or exited — the tab may be a plain shell now). Pick an agent session or a new run.';

export const NON_CLAUDE_AGENT_REASON =
	'That terminal runs an agent other than Claude Code, which reports no liveness — an action never types into it. Pick a Claude session or a new run.';

export const PERMISSION_PENDING_REASON =
	'That terminal is waiting on a permission request — typing into it now would answer it. Decide the request first.';

export const BANG_PROMPT_REASON =
	'The prompt starts a line with "!", which the agent runs as a shell command — refused.';

export const PLAIN_TERMINAL_REASON =
	'The Companion target is a plain terminal — an action only dispatches to an agent. Pick an agent session or a new run.';

function settled(result: ChiRunResult): string {
	if (result.status === 'failed' && result.error) throw new Error(result.error);
	return result.run_id;
}

/** Sends `prompt` to the resolved Chi target and returns the run id. */
export async function send(request: ChiSendRequest): Promise<ChiSendResult> {
	const target = companionTargetFor(request.target, request.engineId);
	const resolved = resolveTarget(target);
	switch (resolved.kind) {
		case 'none':
			throw new ChiUnavailableError('no-engine', resolved.disabledReason ?? 'No Chi target is available');
		case 'pty': {
			// DEC-55 (module note): personal only, a live Claude agent terminal
			// only, no pending permission, no control characters from values,
			// no `!` line.
			if (request.scope !== 'personal') throw new ChiUnavailableError('no-target', PTY_SCOPE_REASON);
			if (target.kind !== 'session' || !isAgentTerminal(target.session_id)) {
				throw new ChiUnavailableError('no-target', PLAIN_TERMINAL_REASON);
			}
			if (isNonClaudeAgent(target.session_id)) {
				throw new ChiUnavailableError('no-target', NON_CLAUDE_AGENT_REASON);
			}
			if (!isAgentLive(target.session_id)) throw new ChiUnavailableError('no-target', AGENT_NOT_LIVE_REASON);
			if (hasPendingPermission(target.session_id)) {
				throw new ChiUnavailableError('permission-pending', PERMISSION_PENDING_REASON);
			}
			const text = request.ptyPrompt ?? request.prompt;
			if (isBangPrompt(text)) throw new ChiUnavailableError('bang-prompt', BANG_PROMPT_REASON);
			// The dispatch path's own PTY write (context line omitted: the
			// action's template is the whole message).
			await resolved.send(text);
			return { runId: null, via: 'pty' };
		}
		case 'chi-resume': {
			if (target.kind !== 'session') throw new ChiUnavailableError('no-target', 'No session to resume');
			return { runId: settled(await chiResume(target.session_id, request.prompt)), via: 'chi-resume' };
		}
		case 'chi-run': {
			const engineId = resolved.engineId;
			if (!engineId) throw new ChiUnavailableError('no-engine', 'No engine installed — open Ngwa → Store');
			const cwd = useShellStore.getState().activeProject.root_path;
			const result = await chiRun({
				engineId,
				prompt: request.prompt,
				...(cwd ? { cwd } : {}),
				persistent: target.kind === 'persistent',
			});
			return { runId: settled(result), via: 'chi-run' };
		}
	}
}

/** Skill names this adapter will put into a prompt (no whitespace / newlines). */
export const SKILL_NAME_RE = /^[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}$/;

/**
 * The prompt that invokes a skill: its slash invocation, the form the
 * Claude Code engine (the default) resolves to an installed skill, followed
 * by optional arguments.
 */
export function skillPrompt(skill: string, args?: string): string {
	const tail = args?.trim();
	return tail ? `/${skill} ${tail}` : `/${skill}`;
}

/** A Chi dispatch invoking `skill`; returns the run id. */
export async function invokeSkill(request: ChiSkillRequest): Promise<ChiSendResult> {
	if (!SKILL_NAME_RE.test(request.skill)) {
		throw new ChiUnavailableError('no-target', `“${request.skill}” is not a valid skill name`);
	}
	const prompt = skillPrompt(request.skill, request.args);
	return send({
		prompt,
		// `args` is free text: it gets the same PTY treatment as a value.
		ptyPrompt: skillPrompt(request.skill, request.args === undefined ? undefined : stripPtyControls(request.args)),
		target: request.target,
		...(request.engineId ? { engineId: request.engineId } : {}),
		scope: request.scope,
	});
}
