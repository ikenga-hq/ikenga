// Onboarding wizard layout route.
//
// All step routes live under this — `/onboarding/welcome`, `/onboarding/
// engine`, `/onboarding/project`, `/onboarding/equipment`, `/onboarding/
// look`, `/onboarding/shortcuts`, `/onboarding/done` (D-04 re-map, WP-38).
// Edge-to-edge full window, which means we don't render the workspace
// activity bar / sidebar / dock at all when we're inside the wizard.
//
// We rely on TanStack's parent layout `Outlet` here. The workspace shell
// itself can detect the `/onboarding` prefix on its own (see boot-redirect
// in `__root.tsx`); this route only handles the in-wizard rendering.

import { Outlet, createFileRoute, redirect } from '@tanstack/react-router';
import { useState } from 'react';

import { useShellStore, ONBOARDING_STEPS } from '@/lib/shell/shell-store';
import { primeOnboardingResumeFlag } from '@/shell/onboarding/wizard-stepper';

export const Route = createFileRoute('/onboarding')({
	beforeLoad: ({ location }) => {
		const state = useShellStore.getState();
		// First-run finished? Don't let stale pane tabs or deep links drag the
		// user back into the wizard. Edit-mode revisits keep working because
		// `enterOnboardingEdit` flips `mode` to 'edit' first.
		if (state.onboarding.mode === 'first_run' && state.onboarding.completedAt !== null) {
			throw redirect({ to: '/' });
		}
		// `/onboarding` (no step) → route to the current active step.
		if (location.pathname === '/onboarding' || location.pathname === '/onboarding/') {
			const idx = Math.min(Math.max(0, state.onboarding.activeIndex), ONBOARDING_STEPS.length - 1);
			const activeId = ONBOARDING_STEPS[idx]!;
			throw redirect({ to: `/onboarding/${activeId}` });
		}
	},
	component: OnboardingLayout,
});

function OnboardingLayout() {
	// This layout mounts once for the whole `/onboarding/*` subtree (unlike
	// each step's <WizardStepper>, which remounts per step) — the one place
	// to decide, per app session, whether this is a "resume" (WP-38 D-04
	// `resume` state: wizard reopened half-done). See the doc comment on
	// `primeOnboardingResumeFlag` in `wizard-stepper.tsx`.
	//
	// A lazy `useState` initialiser, not `useEffect`: the flag must be primed
	// before the child <WizardStepper> reads it in its own first render, and
	// a parent's effects only run after its children have rendered. The
	// initialiser runs exactly once per mount (and the function is idempotent
	// besides, so StrictMode's double-invoke is harmless) — unlike `useMemo`,
	// which React may discard and re-run, and which isn't for side effects.
	useState(() => {
		primeOnboardingResumeFlag();
		return true;
	});
	// Edge-to-edge: just an Outlet. The step bodies wrap themselves in
	// <WizardStepper> which provides the chrome.
	return <Outlet />;
}
