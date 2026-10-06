import { useEffect } from 'react';
import { Outlet, createRootRoute, useLocation, useNavigate } from '@tanstack/react-router';

import { Workspace } from '@/shell/workspace';
import { usePaneScope } from '@/shell/panes/views/route-view';
import { OnboardingIykeMount } from '@/shell/onboarding-iyke-mount';
import { startIykeTimerBridge } from '@/lib/notifications/iyke-timer-bridge';
import { useShellStore } from '@/lib/shell/shell-store';

function RootRoute() {
	// When this same root component renders inside a pane's memory router,
	// `usePaneScope` returns the pane id. We must only emit <Outlet /> in
	// that case — rendering Workspace again would recursively mount the
	// entire shell inside every route pane.
	const paneScope = usePaneScope();
	const location = useLocation();
	const navigate = useNavigate();
	const onboardingMode = useShellStore((s) => s.onboarding.mode);
	const onboardingCompletedAt = useShellStore((s) => s.onboarding.completedAt);

	// Phase 9 (ACP migration): start the OS notification + sidebar badge
	// dispatcher exactly once for the top-level shell. The bridge is
	// idempotent (refcounted) so StrictMode's double-mount and HMR
	// reloads don't create duplicate listeners. We deliberately gate on
	// `paneScope === null` because pane-internal RootRoute remounts
	// would otherwise call this on every focus toggle.
	useEffect(() => {
		if (paneScope !== null) return;
		const stopTimer = startIykeTimerBridge();
		return () => {
			stopTimer();
		};
	}, [paneScope]);

	// Phase 3 boot redirect — first-run users whose wizard hasn't completed
	// get bounced to `/onboarding`. We only do this in the top-level shell
	// (paneScope === null) because individual panes are workspace-internal
	// and shouldn't reroute the whole window.
	useEffect(() => {
		if (paneScope !== null) return;
		if (location.pathname.startsWith('/onboarding')) return;
		if (onboardingMode === 'first_run' && onboardingCompletedAt === null) {
			void navigate({ to: '/onboarding' });
		}
	}, [paneScope, location.pathname, navigate, onboardingMode, onboardingCompletedAt]);

	if (paneScope !== null) {
		// Rendered inside a pane router. Give the route the same bounded h-full
		// flex column the main-window branch gets (via content-pane.tsx's
		// `<main className="flex h-full …">`). Without it, a fill-the-pane route
		// like a pkg iframe (`height:100%`) has no definite-height ancestor, so
		// it grows to its full content height and the pane scrolls as one slab
		// instead of the route scrolling internally.
		return (
			<div className="flex h-full min-h-0 flex-col overflow-hidden">
				<Outlet />
			</div>
		);
	}

	// Onboarding renders edge-to-edge — bypass the Workspace chrome.
	if (location.pathname.startsWith('/onboarding')) {
		return (
			<div className="flex h-screen w-screen flex-col overflow-hidden bg-background text-foreground">
				<OnboardingIykeMount path={location.pathname} />
				<Outlet />
			</div>
		);
	}

	return <Workspace />;
}

export const Route = createRootRoute({
	component: RootRoute,
});
