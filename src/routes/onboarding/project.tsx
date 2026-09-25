// Step 3 — D-04 `project` (the container). Retires `/onboarding/roots`.

import { createFileRoute } from '@tanstack/react-router';

import { ProjectBody } from '@/shell/onboarding/project-body';
import { WizardStepper } from '@/shell/onboarding/wizard-stepper';

export const Route = createFileRoute('/onboarding/project')({
	component: ProjectStep,
});

function ProjectStep() {
	return (
		<WizardStepper stepId="project">
			{({ goNext }) => <ProjectBody onContinue={goNext} />}
		</WizardStepper>
	);
}
