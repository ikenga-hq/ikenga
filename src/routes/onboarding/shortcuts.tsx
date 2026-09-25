// Step 6 — D-04 `shortcuts` (five keys that matter). `new` — no shipped
// equivalent (WP-38).

import { createFileRoute } from '@tanstack/react-router';

import { ShortcutsBody } from '@/shell/onboarding/shortcuts-body';
import { WizardStepper } from '@/shell/onboarding/wizard-stepper';

export const Route = createFileRoute('/onboarding/shortcuts')({
	component: ShortcutsStep,
});

function ShortcutsStep() {
	return (
		<WizardStepper stepId="shortcuts">
			{({ goNext }) => <ShortcutsBody onContinue={goNext} />}
		</WizardStepper>
	);
}
