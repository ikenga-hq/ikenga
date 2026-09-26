// D-06 shared helper (WP-57): what an action's `run` does, in one label plus
// the detail rows under it — the Actions list's Runs column and the detail
// pane's Overview tab share one formatting engine so they never drift.
//
// Erratum §11.3: the package `dispatch` run kind only fills the Companion
// dispatch bar against the current target — it does not send. The `chi` run
// kind (personal/project actions only) is the one that actually dispatches.
// "Dispatch to Chi" belongs to `chi`; `dispatch` reads "Fill the dispatch
// bar…" so the two are never confused in the UI.

import type { EffectiveAction } from '@/lib/actions/store';

export interface RunSummary {
	label: string;
	rows: Array<[string, string]>;
}

export function runSummary(action: EffectiveAction): RunSummary {
	const run = action.run;
	switch (run.kind) {
		case 'builtin':
			return { label: 'Built-in behaviour', rows: [] };
		case 'dispatch':
			return {
				label: 'Fill the dispatch bar…',
				rows: [
					['prompt', run.prompt],
					...(run.target ? ([['target', run.target]] as Array<[string, string]>) : []),
				],
			};
		case 'view':
			return { label: 'Open view', rows: [['route', run.route]] };
		case 'chi':
			return {
				label: 'Dispatch to Chi',
				rows: [
					['target', run.target],
					...(run.engineId ? ([['engine', run.engineId]] as Array<[string, string]>) : []),
					['prompt', run.prompt],
				],
			};
		case 'shell':
			return {
				label: 'Shell command',
				rows: [
					['command', run.command],
					...(run.cwd ? ([['cwd', run.cwd]] as Array<[string, string]>) : []),
					['confirm before run', run.confirm ? 'yes' : 'no'],
				],
			};
		case 'iyke':
			return { label: 'iyke route', rows: [['route', run.route], ['method', run.method ?? 'GET']] };
		case 'skill':
			return { label: 'Skill', rows: [['skill', run.skill]] };
		case 'workflow':
			return { label: 'Workflow', rows: [['workflow', run.workflow]] };
		case 'open':
			return { label: 'Open URL / view', rows: [['url', run.url]] };
		default:
			return { label: 'Unknown run kind', rows: [] };
	}
}

/** The short text for the Actions list's Runs column. */
export function runText(action: EffectiveAction): string {
	return runSummary(action).label;
}
