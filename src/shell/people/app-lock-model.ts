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

/** How often a window may report activity to Rust. The idle window is at
 *  least a minute, so a 30 s throttle can make a lock land at most 30 s late
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
