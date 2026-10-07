// honest-failure-states WP-2 — running a WSL network fix from any surface
// (pane banner, Settings › Engines, the `fix.wsl_network` notification).
//
// `repair_dns` restarts nothing, so it runs at once. `restart_networking`
// (D-1: one UAC prompt) and `switch_to_nat` (D-6) shut every WSL distro down,
// so they open a confirm first (`<WslFixDialogHost>`), listing the sessions
// that will close. On confirm (D-5):
//   1. snapshot {tabId → claudeSessionId} for every WSL tab — BEFORE the
//      call, because `openTabPty`'s exit handler clears the id on exit;
//   2. run the fix;
//   3. `done`: wait for the killed PTYs to report their exit, put each tab's
//      resume id back and respawn it through `openTabPty` (`--resume`);
//      `cancelled_by_user`: the UAC prompt was declined, nothing changed —
//      say so quietly and leave the sessions alone;
//      `failed`: say why, and leave the sessions alone (no respawn into a
//      WSL that just failed to come back).

import { type WslFixAction, type WslFixOutcome, wslHealthFix } from '@/lib/tauri-cmd';
import { openTabPty, useTerminalStore } from '@/terminal/session-store';
import { wslHealthCopy } from './copy';
import { setWslHealth } from './query';
import { useWslHealthUi } from './store';
import {
	normalizeWslDistro,
	planWslRelaunch,
	snapshotWslSessions,
	type WslSessionSnapshot,
	wslShutdownSettled,
} from './tabs';

/** How long to wait for the shutdown's PTY exits to land before relaunching. */
const SHUTDOWN_SETTLE_MS = 10_000;

function errText(e: unknown): string {
	if (e instanceof Error) return e.message;
	if (typeof e === 'string') return e;
	return String(e);
}

function isDisruptive(action: WslFixAction): boolean {
	return action === 'restart_networking' || action === 'switch_to_nat';
}

/** Start a fix: runs `repair_dns` at once, opens the confirm for the rest. */
export function requestWslFix(action: WslFixAction, distro: string | null | undefined): void {
	const key = normalizeWslDistro(distro);
	if (!isDisruptive(action)) {
		void runWslFix(action, key);
		return;
	}
	useWslHealthUi.getState().setConfirm({
		action,
		distro: key,
		sessions: snapshotWslSessions(useTerminalStore.getState().tabs),
	});
}

/** The user confirmed the pending disruptive fix. */
export async function confirmWslFix(): Promise<void> {
	const pending = useWslHealthUi.getState().confirm;
	if (!pending) return;
	useWslHealthUi.getState().setConfirm(null);
	// Re-snapshot at the moment of the call: the confirm may have been open
	// for a while, and a tab's resume id can change in between.
	const snapshot = snapshotWslSessions(useTerminalStore.getState().tabs);
	await runWslFix(pending.action, pending.distro, snapshot);
}

function waitForShutdown(snapshot: readonly WslSessionSnapshot[]): Promise<void> {
	return new Promise((resolve) => {
		if (wslShutdownSettled(snapshot, useTerminalStore.getState().tabs)) {
			resolve();
			return;
		}
		const timer = setTimeout(done, SHUTDOWN_SETTLE_MS);
		const unsub = useTerminalStore.subscribe((s) => {
			if (wslShutdownSettled(snapshot, s.tabs)) done();
		});
		function done() {
			clearTimeout(timer);
			unsub();
			resolve();
		}
	});
}

/**
 * Bring back the sessions a WSL shutdown killed, resuming each Claude
 * conversation from the snapshot. Returns how many tabs were relaunched.
 */
export async function relaunchWslSessions(
	snapshot: readonly WslSessionSnapshot[]
): Promise<number> {
	if (snapshot.length === 0) return 0;
	await waitForShutdown(snapshot);
	const store = useTerminalStore.getState();
	const steps = planWslRelaunch(snapshot, store.tabs);
	let relaunched = 0;
	for (const step of steps) {
		if (step.claudeSessionId) store.setClaudeSessionId(step.tabId, step.claudeSessionId);
		// `spawning` clears the agent-live flag the id just set and is the
		// state a mounted SingleTerminal respawns from; `openTabPty` is
		// single-flight, so this call and that one share one PTY.
		store.setStatus(step.tabId, 'spawning');
		const tab = useTerminalStore.getState().tabs.find((t) => t.id === step.tabId);
		if (!tab) continue;
		try {
			await openTabPty(tab);
			relaunched += 1;
		} catch (err) {
			console.error('[wsl-health] relaunch failed for', step.tabId, err);
			useTerminalStore.getState().setStatus(step.tabId, 'error');
		}
	}
	return relaunched;
}

function doneMessage(
	action: WslFixAction,
	outcome: Extract<WslFixOutcome, { outcome: 'done' }>,
	relaunched: number
): string {
	const reopened =
		relaunched > 0 ? ` Reopened ${relaunched} WSL session${relaunched === 1 ? '' : 's'}.` : '';
	if (outcome.health.state === 'ok') {
		const what =
			action === 'switch_to_nat'
				? 'Switched to NAT — WSL is back online.'
				: 'Fixed — WSL is back online.';
		return `${what}${reopened}`;
	}
	const copy = wslHealthCopy(outcome.health);
	return `That didn't fix it${copy ? `: ${copy.title.charAt(0).toLowerCase()}${copy.title.slice(1)}` : ''}.${reopened}`;
}

/** Run a fix now (no confirm). `snapshot` = the sessions to relaunch. */
export async function runWslFix(
	action: WslFixAction,
	distro: string | null | undefined,
	snapshot: readonly WslSessionSnapshot[] = []
): Promise<WslFixOutcome> {
	const key = normalizeWslDistro(distro);
	const ui = useWslHealthUi.getState();
	ui.setRun(key, { action, phase: 'running', message: null, at: Date.now() });

	let outcome: WslFixOutcome;
	try {
		outcome = await wslHealthFix(action, key);
	} catch (e) {
		outcome = { outcome: 'failed', reason: errText(e) };
	}

	switch (outcome.outcome) {
		case 'done': {
			setWslHealth(key, outcome.health);
			if (action === 'restart_networking' && outcome.health.state !== 'ok') {
				useWslHealthUi.getState().markRestartTried(key);
			}
			let relaunched = 0;
			if (isDisruptive(action) && snapshot.length > 0) {
				useWslHealthUi
					.getState()
					.setRun(key, { action, phase: 'relaunching', message: null, at: Date.now() });
				relaunched = await relaunchWslSessions(snapshot);
			}
			useWslHealthUi.getState().setRun(key, {
				action,
				phase: 'done',
				message: doneMessage(action, outcome, relaunched),
				at: Date.now(),
			});
			break;
		}
		case 'cancelled_by_user':
			useWslHealthUi.getState().setRun(key, {
				action,
				phase: 'cancelled',
				message: 'Cancelled at the administrator prompt — nothing was changed.',
				at: Date.now(),
			});
			break;
		case 'failed':
			useWslHealthUi.getState().setRun(key, {
				action,
				phase: 'failed',
				message: `Couldn't fix it: ${outcome.reason}`,
				at: Date.now(),
			});
			break;
	}
	return outcome;
}
