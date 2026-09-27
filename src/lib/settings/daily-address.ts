// WP-39 / D-04 — `workspace.dailyAddress` (settings.json, personal-only,
// default `true`). Read by the daily address on the Project dashboard and
// written from Settings › Workspace. Dependency-free so both sides (and their
// tests) can use it without the Tauri runtime.

import type { SettingsDocument } from './types';

export const DAILY_ADDRESS_FIELD = 'workspace.dailyAddress' as const;

/** On unless the effective document says `false` (missing = default on). */
export function isDailyAddressEnabled(document: SettingsDocument | null | undefined): boolean {
	return document?.workspace?.dailyAddress !== false;
}
