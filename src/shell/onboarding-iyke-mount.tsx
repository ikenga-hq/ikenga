import { useEffect } from 'react';

import { useIykeBridge } from '@/lib/iyke/bridge';
import { setShell } from '@/lib/iyke/client';
import { isTauri } from '@/lib/transport';

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
 *
 * The `iyke_set_shell` push is desktop-only (gap audit rank 25): the iyke
 * control bridge is the desktop app's localhost server and the headless
 * daemon does not serve `iyke_set_shell`, so in a browser every onboarding
 * step fired a failing RPC. `desktop` defaults to `isTauri()`, which is fixed
 * for a page's life; it is a prop only so tests can pin either side.
 */
export function OnboardingIykeMount({
	path,
	desktop = isTauri(),
}: {
	path: string;
	desktop?: boolean;
}) {
	useIykeBridge();
	useEffect(() => {
		if (!desktop) return;
		setShell({ mode: 'onboarding', route: path, panes: null, sidebarCollapsed: true }).catch(
			(err) => {
				console.warn('[iyke] onboarding set_shell failed:', err);
			}
		);
	}, [path, desktop]);
	return null;
}
