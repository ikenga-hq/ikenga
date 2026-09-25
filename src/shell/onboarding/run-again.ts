// "Run consecration again" — the one reset-onboarding path, shared by
// Settings › Workspace (`src/routes/settings/workspace.tsx`) and the daily
// address footer (`src/shell/home/daily-address.tsx`, WP-39 / D-04). Confirms,
// resets the wizard state on the shell store, then hands off to the caller's
// navigation (both callers route to `/onboarding`).

import { useShellStore } from '@/lib/shell/shell-store';
import { confirm as confirmDialog } from '@/lib/transport/dialog-shim';

/** Returns whether the reset ran (false when the user cancelled). */
export async function runConsecrationAgain(goToOnboarding: () => void): Promise<boolean> {
	const ok = await confirmDialog(
		'Reset onboarding and re-run every step? Your existing workspace settings stay put — this only re-opens the wizard.',
		{ title: 'Run consecration again', kind: 'info' }
	);
	if (!ok) return false;
	useShellStore.getState().resetOnboarding();
	goToOnboarding();
	return true;
}
