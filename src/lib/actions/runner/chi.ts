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
// (`via: 'pty'`). It is only allowed into an AGENT terminal (a `wrap` spec —
// the same test `resolve-target.ts` uses for its context line): an action
// run never types into a plain shell, where `text + "\r"` would execute as a
// command and bypass the DEC-55 gate for an untrusted project `chi` action.
// A plain-shell active target is refused with `no-target`. The manifest
// `dispatch` kind and built-in "Hand to Chi" stay fill-only and never come
// through here.

import { chiResume, chiRun, type ChiRunResult } from '@/lib/tauri-cmd';
import { useShellStore, type CompanionTarget } from '@/lib/shell/shell-store';
import { resolveTarget } from '@/shell/companion/resolve-target';
import { useTerminalStore } from '@/terminal/session-store';
import type { ChiTarget } from '../types';

export interface ChiSendRequest {
	/** Final text (already interpolated, raw — §8.2). */
	prompt: string;
	target: ChiTarget;
	/** Required iff `target === 'engine'`. */
	engineId?: string;
}

export interface ChiSkillRequest {
	skill: string;
	/** Free text after the skill invocation; optional. */
	args?: string;
	target: ChiTarget;
	engineId?: string;
}

export interface ChiSendResult {
	/** The Chi run id; `null` when the text was injected into a live PTY. */
	runId: string | null;
	via: 'chi-run' | 'chi-resume' | 'pty';
}

/** Nothing to send to — the typed reason the runner reports. */
export class ChiUnavailableError extends Error {
	readonly reason: 'no-engine' | 'no-target';

	constructor(reason: 'no-engine' | 'no-target', message: string) {
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
		case 'pty':
			// DEC-55: never type into a raw shell — only into an agent terminal.
			if (target.kind !== 'session' || !isAgentTerminal(target.session_id)) {
				throw new ChiUnavailableError('no-target', PLAIN_TERMINAL_REASON);
			}
			// The dispatch path's own PTY write (context line omitted: the
			// action's template is the whole message).
			await resolved.send(request.prompt);
			return { runId: null, via: 'pty' };
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
	return send({
		prompt: skillPrompt(request.skill, request.args),
		target: request.target,
		...(request.engineId ? { engineId: request.engineId } : {}),
	});
}
