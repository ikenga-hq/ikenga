// honest-failure-states WP-2 — which terminal tabs run inside WSL, which
// distro they run in, and the D-5 relaunch bookkeeping around a fix that
// shuts WSL down. Pure: no store, no Tauri, so it is unit-tested directly.
//
// D-5 gotcha: `openTabPty`'s exit handler clears `claudeSessionId` the moment
// `wsl --shutdown` kills a tab's PTY, so the resume ids must be snapshotted
// BEFORE the fix runs and put back before the respawn.

import type { TerminalTab } from '@/terminal/session-store';

/** The distro key the shell uses everywhere: a trimmed name, or `'default'`
 *  for the unnamed default distro. Never `null` — the backend reads `null` as
 *  `engines.agentWslDistro`, which is not necessarily the tab's distro. */
export function normalizeWslDistro(distro: string | null | undefined): string {
	const name = distro?.trim();
	if (!name || name.toLowerCase() === 'default') return 'default';
	return name;
}

/** Human label for a distro key. */
export function wslDistroLabel(distro: string | null | undefined): string {
	const key = normalizeWslDistro(distro);
	return key === 'default' ? 'the default distro' : key;
}

function isWslExe(arg: string | undefined): boolean {
	if (!arg) return false;
	const base = arg.split(/[\\/]/).pop() ?? '';
	return /^wsl(\.exe)?$/i.test(base);
}

/** True when the tab's process runs inside WSL: an agent wrapped for the
 *  `wsl` target, or a plain shell tab whose command is `wsl.exe`. */
export function isWslTab(tab: Pick<TerminalTab, 'spec'>): boolean {
	if (tab.spec.wrap?.shellTarget === 'wsl') return true;
	return isWslExe(tab.spec.cmd[0]);
}

/** The distro key a WSL tab runs in (`'default'` when unnamed). */
export function wslTabDistro(tab: Pick<TerminalTab, 'spec'>): string {
	if (tab.spec.wrap?.shellTarget === 'wsl') return normalizeWslDistro(tab.spec.wrap.wslDistro);
	const cmd = tab.spec.cmd;
	if (isWslExe(cmd[0])) {
		for (let i = 1; i < cmd.length - 1; i++) {
			if (cmd[i] === '-d' || cmd[i] === '--distribution') return normalizeWslDistro(cmd[i + 1]);
			// Everything after `-e` / `--exec` / `--` belongs to the command.
			if (cmd[i] === '-e' || cmd[i] === '--exec' || cmd[i] === '--') break;
		}
	}
	return 'default';
}

/** One WSL tab as it was just before a WSL-wide shutdown. */
export interface WslSessionSnapshot {
	tabId: string;
	title: string;
	distro: string;
	/** The Claude conversation to resume (`--resume`), if any. */
	claudeSessionId: string | null;
	/** Whether its process was running — only those are relaunched. */
	wasRunning: boolean;
}

/** Snapshot every WSL tab (all distros: `wsl --shutdown` stops every one). */
export function snapshotWslSessions(tabs: readonly TerminalTab[]): WslSessionSnapshot[] {
	return tabs.filter(isWslTab).map((t) => ({
		tabId: t.id,
		title: t.title,
		distro: wslTabDistro(t),
		claudeSessionId: t.claudeSessionId ?? null,
		wasRunning: t.status === 'running' || t.status === 'spawning' || Boolean(t.ptyId),
	}));
}

/** What to relaunch once the shutdown is over. */
export interface WslRelaunchStep {
	tabId: string;
	claudeSessionId: string | null;
}

/**
 * The tabs to relaunch after a fix shut WSL down: those that were running at
 * snapshot time, still exist, and are no longer running (the shutdown killed
 * them). A tab the shutdown didn't kill, or one the user closed meanwhile, is
 * left alone. The resume id comes from the snapshot, since the exit handler
 * has cleared it on the live tab.
 */
export function planWslRelaunch(
	snapshot: readonly WslSessionSnapshot[],
	tabsNow: readonly Pick<TerminalTab, 'id' | 'status' | 'ptyId'>[]
): WslRelaunchStep[] {
	const byId = new Map(tabsNow.map((t) => [t.id, t]));
	const steps: WslRelaunchStep[] = [];
	for (const snap of snapshot) {
		if (!snap.wasRunning) continue;
		const now = byId.get(snap.tabId);
		if (!now) continue;
		const dead = (now.status === 'exited' || now.status === 'error') && !now.ptyId;
		if (!dead) continue;
		steps.push({ tabId: snap.tabId, claudeSessionId: snap.claudeSessionId });
	}
	return steps;
}

/** True once every running snapshot tab has stopped (or gone). */
export function wslShutdownSettled(
	snapshot: readonly WslSessionSnapshot[],
	tabsNow: readonly Pick<TerminalTab, 'id' | 'status' | 'ptyId'>[]
): boolean {
	const byId = new Map(tabsNow.map((t) => [t.id, t]));
	return snapshot.every((snap) => {
		if (!snap.wasRunning) return true;
		const now = byId.get(snap.tabId);
		return !now || now.status === 'exited' || now.status === 'error';
	});
}
