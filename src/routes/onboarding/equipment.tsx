// Step 4 — D-04 `equipment` (gather your Ngwa). Retires `/onboarding/
// packages`, `/onboarding/connectors` and `/onboarding/scaffolding` — see
// `equipment-body.tsx`'s header comment and the WP-38 PR body for the
// merge's write-map.
//
// Owns the registry-reachability probe for the D-04 `offline` chrome state
// (`designs/onboarding.html`: "registry unreachable — suggested packages
// disabled with explanation, rest continues").

import { useQuery } from '@tanstack/react-query';
import { createFileRoute } from '@tanstack/react-router';

import { fetchIndex } from '@/lib/registry/client';
import { EquipmentBody } from '@/shell/onboarding/equipment-body';
import { WizardStepper } from '@/shell/onboarding/wizard-stepper';

export const Route = createFileRoute('/onboarding/equipment')({
	component: EquipmentStep,
});

function EquipmentStep() {
	const registry = useQuery({
		queryKey: ['onboarding', 'registry-reachable'],
		queryFn: () => fetchIndex(),
		retry: false,
		staleTime: 60_000,
		refetchOnWindowFocus: false,
	});
	const isOffline = registry.isError;

	return (
		<WizardStepper stepId="equipment" stateOverride={isOffline ? 'offline' : undefined}>
			{({ goNext }) => (
				<EquipmentBody onContinue={goNext} stateOverride={isOffline ? 'offline' : undefined} />
			)}
		</WizardStepper>
	);
}
