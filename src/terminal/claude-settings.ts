// The `--settings` path Ikenga hands `claude` in the terminals it launches.
//
// Mirrors the prime-once + sync-getter shape of `@/lib/home`, and for the same
// reason: `buildAgentArgs` is synchronous and called from three render paths
// (workspace, dock, command palette), so it cannot await an IPC round-trip.
//
// The path is produced Rust-side at PTY-spawn (`iyke::hook_settings`), but the
// frontend and Rust compute the same deterministic path from the terminal id
// and the app-local data directory. The actual file content (live port + token)
// is written by Rust right before the child is exec'd. This keeps attribution
// exact: every hook event POSTs to `/iyke/hooks/event?terminal=<id>`.
//
// If priming hasn't finished, or the file couldn't be written, the getter
// returns null and the wrapper omits `--settings`: the session runs normally,
// just without the shell's live view of it.

import { getEndpoint } from '@/lib/iyke/client';

let cachedAppDataDir: string | null = null;
/** Distinguishes "primed, and the answer is null" from "never primed". */
let primed = false;
let inFlight: Promise<string | null> | null = null;

export function loadClaudeSettingsPath(): Promise<string | null> {
	if (primed) return Promise.resolve(cachedAppDataDir);
	if (!inFlight) {
		inFlight = getEndpoint()
			.then((ep) => {
				cachedAppDataDir = ep.app_local_data_dir || null;
				primed = true;
				return cachedAppDataDir;
			})
			.catch(() => {
				// Do NOT mark primed. `iyke_endpoint` rejects with "iyke runtime not
				// initialized" if it is called before the bridge binds, and memoising
				// that would disable `--settings` for the whole session over a race
				// measured in milliseconds. Clearing `inFlight` lets the next call
				// retry — same reasoning as `lib/sql-db.ts`.
				inFlight = null;
				return null;
			});
	}
	return inFlight;
}

/**
 * Synchronous read for argv construction. Returns null until primed, and
 * kicks off a prime when it finds itself cold so the next launch has it.
 *
 * When `terminalId` is provided, returns the per-terminal settings file path;
 * otherwise returns the legacy shared path (used for tests / old callers).
 */
export function getClaudeSettingsPathSync(terminalId?: string): string | null {
	if (!primed) void loadClaudeSettingsPath();
	if (!cachedAppDataDir) return null;
	if (terminalId) {
		return `${cachedAppDataDir}/claude-hooks-${terminalId}.json`;
	}
	return `${cachedAppDataDir}/claude-hooks-settings.json`;
}

/**
 * Per-terminal settings path. Same as `getClaudeSettingsPathSync(terminalId)`
 * but makes the intent explicit.
 */
export function getClaudeSettingsPathForTerminal(terminalId: string): string | null {
	return getClaudeSettingsPathSync(terminalId);
}

/** Test seam — lets the argv tests assert both the primed and unprimed shapes. */
export function __setClaudeSettingsPathForTests(path: string | null): void {
	if (!path) {
		cachedAppDataDir = null;
		primed = false;
	} else {
		// Legacy test seam may pass the shared file path or a directory.
		cachedAppDataDir = path.endsWith('.json')
			? path
					.replace(/\/claude-hooks-[^/]+\.json$/, '')
					.replace(/\/claude-hooks-settings\.json$/, '')
			: path;
		primed = true;
	}
	inFlight = null;
}
