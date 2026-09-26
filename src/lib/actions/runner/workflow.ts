// WP-53 — the `workflow` run kind (G-ACTIONS §8.1): disabled with a typed
// reason until a workflow runner exists (out of scope, `01` §Phase 6). A
// project-scope workflow action also sits behind the DEC-55 trust gate, so
// an untrusted one reports the trust refusal first (§8.3: "refuses (and
// disabled)").

import type { ActionRun } from '../types';

export type WorkflowRun = Extract<ActionRun, { kind: 'workflow' }>;

export const NO_WORKFLOW_RUNNER_REASON = 'Workflows cannot run yet — there is no workflow runner.';

export interface WorkflowDisabled {
	reason: 'no-workflow-runner';
	message: string;
	workflow: string;
}

export function workflowDisabled(run: WorkflowRun): WorkflowDisabled {
	return { reason: 'no-workflow-runner', message: NO_WORKFLOW_RUNNER_REASON, workflow: run.workflow };
}
