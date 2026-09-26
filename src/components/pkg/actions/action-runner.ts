// ActionRunner (WP-5 · WP-18b) — central dispatch for skill-action `ux_mode`s.
//
// WP-53: a skill action's prompt is its skill invocation (`/<skill> <verb>`).
// What happens to it depends on `ux_mode`:
//
//   • `confirm` / `approve` (and the well-known `setup`) — FILL: the prompt
//     seeds the Companion dispatch bar (`handToChi`) and nothing is sent;
//     the user reviews and presses Enter (seed → review → send).
//   • `auto` — SEND now through the action runner (`src/lib/actions/runner`,
//     its `chi` path to the Companion's active target), resolving with the
//     run id (DEC-63.3). `auto` is not a parsed `ux_mode` yet
//     (`skill_actions.rs` `UX_MODES`), so today every dispatch fills.
//
// A package's skill actions are package content, covered by the package's
// own Ngwa trust — the DEC-55 project-file gate does not apply (G-ACTIONS
// §8.3: package actions are never gated). A send runs as scope `package`:
// ungated, but — like a project action — never typed into a terminal (a
// headless Chi run only; `runner/chi.ts`). Package `dispatch` stays fill-only.

import { runAction, type RunOutcome } from '@/lib/actions/runner';
import { skillPrompt } from '@/lib/actions/runner/chi';
import type { SkillAction } from '@/lib/tauri-cmd';
import { handToChi } from '@/shell/companion/companion-store';

/** Result of a dispatch. `ok` = the prompt was filled into the Companion
 *  (`filled: true`) or sent (`runId`, null when typed into a live agent
 *  terminal). */
export interface OpenSessionDialogResult {
	ok: boolean;
	reason?: 'scope-denied' | 'cancelled' | 'not-implemented' | 'unavailable' | 'failed';
	/** Why it did not run (`unavailable` / `failed`). */
	message?: string;
	/** The prompt was seeded into the dispatch bar, not sent. */
	filled?: boolean;
	runId?: string | null;
}

const DISPATCHABLE_UX_MODES = ['confirm', 'approve', 'auto'] as const;

/** The only mode that sends without review. */
const SENDING_UX_MODE = 'auto';

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

/** Dispatch a skill action: fill the Companion (`confirm` / `approve` /
 *  setup) or send through the WP-53 runner (`auto`). Disabled modes never
 *  dispatch (`isDispatchable`). */
export async function dispatchAction(
	action: SkillAction,
	opts: DispatchOptions = {}
): Promise<OpenSessionDialogResult> {
	if (!isDispatchable(action)) return { ok: false, reason: 'not-implemented' };
	const prompt = skillActionPrompt(action, opts);
	if (action.uxMode !== SENDING_UX_MODE) {
		// Seed → review → send: the user presses Enter in the dispatch bar.
		handToChi(prompt);
		return { ok: true, filled: true };
	}
	const outcome = await runAction({
		id: `${action.pkgId}:${action.skill}:${action.verb}`,
		name: action.name,
		run: { kind: 'chi', target: 'active', prompt },
		// Package content: never DEC-55-gated, never PTY-injected.
		scope: 'package',
	});
	return toDispatchResult(outcome);
}
