// Step 7 — D-04 `done` (what was set up). Retires `/onboarding/summary`.

import { createFileRoute } from '@tanstack/react-router';

import { DoneBody } from '@/shell/onboarding/done-body';
import { WizardStepper } from '@/shell/onboarding/wizard-stepper';

export const Route = createFileRoute('/onboarding/done')({
	component: DoneStep,
});

function DoneStep() {
	return (
		<WizardStepper stepId="done">
			{({ goTo, goNext }) => <DoneBody onFinish={goNext} goTo={goTo} />}
		</WizardStepper>
	);
}
