// Step 2 — D-04 `engine` (Chi picker). Retires `/onboarding/agent`.
//
// Owns the single `useAgentDetect` probe so both this route's `engine-none`
// state check and `EngineBody`'s render share one scan of $PATH.

import { createFileRoute } from '@tanstack/react-router';

import { useAgentDetect } from '@/lib/shell/use-agent-detect';
import { EngineBody, SUPPORTED_ENGINE_IDS, computeAllMissing } from '@/shell/onboarding/engine-body';
import { WizardStepper } from '@/shell/onboarding/wizard-stepper';

export const Route = createFileRoute('/onboarding/engine')({
	component: EngineStep,
});

function EngineStep() {
	const { results, refresh } = useAgentDetect(SUPPORTED_ENGINE_IDS);
	const stateOverride = computeAllMissing(results) ? ('engine-none' as const) : undefined;

	return (
		<WizardStepper stepId="engine" stateOverride={stateOverride}>
			{({ goNext }) => <EngineBody onContinue={goNext} results={results} refresh={refresh} />}
		</WizardStepper>
	);
}
