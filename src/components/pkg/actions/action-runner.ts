// ActionRunner (WP-5 · WP-18b) — central dispatch for skill-action `ux_mode`s.
//
// WP-53: `dispatchAction` delegates to the action runner
// (`src/lib/actions/runner`). A skill action is a Chi dispatch invoking its
// skill (`/<skill> <verb>`, the runner's `skill` path) to the Companion's
// active target; it SENDS and resolves with the run id (DEC-63.3). A
// package's skill actions are package content, covered by the package's own
// Ngwa trust — the DEC-55 project-file gate does not apply (G-ACTIONS §8.3:
// package actions are never gated), so they run as an ungated source.

import { runAction, type RunOutcome } from '@/lib/actions/runner';
import { skillPrompt } from '@/lib/actions/runner/chi';
import type { SkillAction } from '@/lib/tauri-cmd';

/** Result of a dispatch. `ok` = the Chi dispatch was sent (`runId` is
 *  null when it was typed into a live terminal). */
export interface OpenSessionDialogResult {
	ok: boolean;
	reason?: 'scope-denied' | 'cancelled' | 'not-implemented' | 'unavailable' | 'failed';
	/** Why it did not run (`unavailable` / `failed`). */
	message?: string;
	runId?: string | null;
}

const DISPATCHABLE_UX_MODES = ['confirm', 'approve'] as const;

/** The well-known `setup` action — the contract's `superRefine` gates the
 *  `setup` block on it, and `skill_actions.rs` derives `verb === name` from the
 *  same frontmatter `name` (else the file stem), so both equal `'setup'` for
 *  `<skill>/actions/setup.md`. Match either for robustness. */
export function isSetupAction(action: Pick<SkillAction, 'name' | 'verb'>): boolean {
	return action.name === 'setup' || action.verb === 'setup';
}

/** Whether the runner can dispatch this action today (button enabled).
 *  `setup` is dispatchable by name regardless of its `streaming` ux_mode;
 *  everything else falls back to the mode allow-list. */
export function isDispatchable(action: SkillAction): boolean {
	if (isSetupAction(action)) return true;
	return (DISPATCHABLE_UX_MODES as readonly string[]).includes(action.uxMode);
}

/** Options for a dispatch. `interview` forces the setup interview flow
 *  (§5) instead of the ai-infer default; ignored for non-setup actions. */
export interface DispatchOptions {
	interview?: boolean;
}

/** The prompt a skill action dispatches: its skill, then its verb. */
export function skillActionPrompt(action: SkillAction, opts: DispatchOptions = {}): string {
	const verb = action.verb && action.verb !== action.skill ? action.verb : '';
	const args = [verb, isSetupAction(action) && opts.interview ? '--interview' : '']
		.filter(Boolean)
		.join(' ');
	return skillPrompt(action.skill, args || undefined);
}

export function toDispatchResult(outcome: RunOutcome): OpenSessionDialogResult {
	switch (outcome.status) {
		case 'done':
			return { ok: true, runId: outcome.runId ?? null };
		case 'refused':
			return outcome.reason === 'cancelled'
				? { ok: false, reason: 'cancelled' }
				: { ok: false, reason: 'unavailable', message: outcome.message };
		case 'failed':
			return { ok: false, reason: 'failed', message: outcome.message };
		case 'preview':
			return { ok: false, reason: 'not-implemented' };
	}
}

/** Dispatch a skill action through the WP-53 runner. Disabled modes never
 *  dispatch (`isDispatchable`). */
export async function dispatchAction(
	action: SkillAction,
	opts: DispatchOptions = {}
): Promise<OpenSessionDialogResult> {
	if (!isDispatchable(action)) return { ok: false, reason: 'not-implemented' };
	const outcome = await runAction({
		id: `${action.pkgId}:${action.skill}:${action.verb}`,
		name: action.name,
		run: { kind: 'chi', target: 'active', prompt: skillActionPrompt(action, opts) },
		// Package content: not a project file, so never DEC-55-gated.
		scope: 'personal',
	});
	return toDispatchResult(outcome);
}
