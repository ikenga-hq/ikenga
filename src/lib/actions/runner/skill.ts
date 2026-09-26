// WP-53 — the `skill` run kind (G-ACTIONS §8.1): a Chi dispatch invoking an
// installed skill, through the narrow adapter in `chi.ts` (Mock contract 3).
// Project-scope skill runs are trust-gated (DEC-55) by the runner before
// they reach here. The skill goes to the Companion's active target; the
// action's scope travels with it, so a non-personal skill never types into
// a PTY (`chi.ts` module note).

import type { ActionRun, ActionsScope, ChiTarget } from '../types';
import { invokeSkill, SKILL_NAME_RE, type ChiSendResult, type ChiSkillRequest } from './chi';

export type SkillRun = Extract<ActionRun, { kind: 'skill' }>;

/** The target a `skill` run dispatches to (the file carries no target). */
export const SKILL_TARGET: ChiTarget = 'active';

export function isValidSkillName(skill: string): boolean {
	return SKILL_NAME_RE.test(skill);
}

export function skillRequest(run: SkillRun, scope: ActionsScope): ChiSkillRequest {
	return { skill: run.skill.trim(), target: SKILL_TARGET, scope };
}

export function runSkill(
	run: SkillRun,
	scope: ActionsScope,
	adapter: (request: ChiSkillRequest) => Promise<ChiSendResult> = invokeSkill
): Promise<ChiSendResult> {
	return adapter(skillRequest(run, scope));
}
