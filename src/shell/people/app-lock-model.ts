// App lock — pure helpers (WP-72, D-05 `profile` App lock block + `locked`).
//
// Mirrors the limits in `src-tauri/src/commands/app_lock.rs`, so the form can
// refuse a bad value before the round trip. Rust re-checks everything; this
// file only decides what the UI says.

import type { AppLockBiometric, AppLockMethod, AppLockStatus } from '@/lib/tauri-cmd';

export const MIN_SECRET_CHARS = 4;
export const MAX_SECRET_CHARS = 256;
export const MIN_IDLE_MINUTES = 1;
export const MAX_IDLE_MINUTES = 24 * 60;
export const DEFAULT_IDLE_MINUTES = 15;

/** How often a window may report activity to Rust. The first input in a
 *  window is sent at once; later input in the same window is sent once, when
 *  the window ends (a trailing send). Rust's last-activity time is therefore
 *  never older than the real last input, so a lock can land at most 30 s late
 *  and never early. */
export const ACTIVITY_THROTTLE_MS = 30_000;

/** Parse the idle-minutes field. `null` when it isn't a whole number in range. */
export function parseIdleMinutes(raw: string): number | null {
	const trimmed = raw.trim();
	if (!/^\d+$/.test(trimmed)) return null;
	const n = Number(trimmed);
	if (!Number.isSafeInteger(n) || n < MIN_IDLE_MINUTES || n > MAX_IDLE_MINUTES) return null;
	return n;
}

/** Why a new PIN / passphrase is refused, or `null` when it's fine. */
export function secretProblem(secret: string): string | null {
	const chars = Array.from(secret).length;
	if (chars < MIN_SECRET_CHARS) return `Use at least ${MIN_SECRET_CHARS} characters.`;
	if (chars > MAX_SECRET_CHARS) return `Use at most ${MAX_SECRET_CHARS} characters.`;
	if (secret.trim().length === 0) return "A PIN can't be only spaces.";
	return null;
}

/** D-05's meta line under "Locked": `ned-desktop · locked after 15 min idle`. */
export function lockMetaLine(status: Pick<AppLockStatus, 'host' | 'reason' | 'idleMinutes'>): string {
	const host = status.host || 'this device';
	switch (status.reason) {
		case 'idle':
			return `${host} · locked after ${status.idleMinutes} min idle`;
		case 'launch':
			return `${host} · locked at launch`;
		case 'manual':
			return `${host} · locked with Lock now`;
		default:
			return `${host} · locked`;
	}
}

export interface UnlockMethodOption {
	id: AppLockMethod;
	label: string;
	disabled: boolean;
	/** Tooltip / screen-reader reason when disabled. */
	why?: string;
}

/**
 * D-05's "Unlock with" choice: the OS option first, then the PIN. The OS
 * option only shows where the platform has one (Windows Hello, Touch ID), and
 * it stays disabled with the reason while this build can't prompt for it.
 * Linux has no OS option at all.
 */
export function unlockMethodOptions(biometric: AppLockBiometric): UnlockMethodOption[] {
	const pin: UnlockMethodOption = { id: 'pin', label: 'PIN or passphrase', disabled: false };
	if (biometric.kind === 'none') return [pin];
	return [
		{
			id: 'os',
			label: biometric.label,
			disabled: !biometric.available,
			why: biometric.available ? undefined : biometric.reason,
		},
		pin,
	];
}

/** "Try again in 12 s". */
export function retryLine(ms: number): string {
	return `Try again in ${Math.max(1, Math.ceil(ms / 1000))} s.`;
}

/** Throttle gate for activity reports. */
export function shouldReportActivity(lastSentMs: number | null, nowMs: number): boolean {
	return lastSentMs === null || nowMs - lastSentMs >= ACTIVITY_THROTTLE_MS;
}

/** How long until the trailing send for a throttle window that began at
 *  `lastSentMs`. */
export function trailingDelay(lastSentMs: number, nowMs: number): number {
	return Math.max(0, lastSentMs + ACTIVITY_THROTTLE_MS - nowMs);
}

/** Chi run states that are still going (the rest have ended). */
const RUN_GOING = new Set(['queued', 'running', 'awaiting_auth']);

/** How many `chi_list` rows are still going. */
export function countGoingRuns(rows: readonly { status: string }[]): number {
	return rows.filter((r) => RUN_GOING.has(r.status)).length;
}

/**
 * D-05 `locked`'s fine print: "**2 sessions and 1 run are still going**
 * underneath." `null` counts (the host couldn't say) keep the plain claim.
 */
export function stillGoingLine(sessions: number | null, runs: number | null): string {
	if (sessions === null || runs === null) return 'Sessions and runs keep going';
	const s = `${sessions} session${sessions === 1 ? '' : 's'}`;
	const r = `${runs} run${runs === 1 ? '' : 's'}`;
	if (sessions > 0 && runs > 0) return `${s} and ${r} are still going`;
	if (sessions > 0) return `${s} ${sessions === 1 ? 'is' : 'are'} still going`;
	if (runs > 0) return `${r} ${runs === 1 ? 'is' : 'are'} still going`;
	return 'No sessions or runs are going';
}
