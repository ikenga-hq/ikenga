// Step 5 — D-04 `look` (theme · mode · density). Retires
// `/onboarding/appearance`.

import { createFileRoute } from '@tanstack/react-router';

import { LookBody } from '@/shell/onboarding/look-body';
import { WizardStepper } from '@/shell/onboarding/wizard-stepper';

export const Route = createFileRoute('/onboarding/look')({
	component: LookStep,
});

function LookStep() {
	return (
		<WizardStepper stepId="look">{({ goNext }) => <LookBody onContinue={goNext} />}</WizardStepper>
	);
}
