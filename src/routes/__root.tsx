import { useEffect } from 'react';
import { Outlet, createRootRoute, useLocation, useNavigate } from '@tanstack/react-router';

import { Workspace } from '@/shell/workspace';
import { usePaneScope } from '@/shell/panes/views/route-view';
import { useIykeBridge } from '@/lib/iyke/bridge';
import { setShell } from '@/lib/iyke/client';
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

/**
 * Keep the shell observable while the onboarding wizard is up (ikenga#147).
 *
 * Onboarding deliberately bypasses `<Workspace />`, and Workspace is what
 * mounts `useIykeBridge` + `useIykeShellSync`. The side effect was that a
 * perfectly healthy build sitting on the wizard was indistinguishable from a
 * dead FE⇄backend channel: `/iyke/dom` timed out and `/iyke/state` reported
 * `mode: null, route: null` — exactly the ikenga#140 signature. That confound
 * invalidated a whole session's reproduction of #140.
 *
 * So: mount the bridge here too (it is a pure set of Tauri event listeners and
 * this branch is mutually exclusive with the Workspace branch, so nothing is
 * mounted twice), and publish a *literal* route. We deliberately do NOT reuse
 * `useIykeShellSync` — it derives the route from the focused pane, which on a
 * first run still holds its default `/` and would therefore report the shell as
 * being on the workspace while the wizard is on screen. Reporting nothing would
 * be bad; reporting the wrong thing confidently would be worse.
 */
function OnboardingIykeMount({ path }: { path: string }) {
	useIykeBridge();
	useEffect(() => {
		setShell({ mode: 'onboarding', route: path, panes: null, sidebarCollapsed: true }).catch(
			(err) => {
				console.warn('[iyke] onboarding set_shell failed:', err);
			}
		);
	}, [path]);
	return null;
}

export const Route = createRootRoute({
	component: RootRoute,
});
