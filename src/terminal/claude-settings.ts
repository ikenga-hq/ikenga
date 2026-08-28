// The `--settings` path Ikenga hands `claude` in the terminals it launches.
//
// Mirrors the prime-once + sync-getter shape of `@/lib/home`, and for the same
// reason: `buildAgentArgs` is synchronous and called from three render paths
// (workspace, dock, command palette), so it cannot await an IPC round-trip.
//
// The path is produced Rust-side at iyke-bridge start (`iyke::hook_settings`),
// the only moment the real port and bearer token both exist. That is
// deliberate — the bug this replaces (ikenga#149) was a settings document baked
// with a placeholder `port: 0`, so every hook fired a curl at a dead port on
// every tool call. Deriving the path from the live endpoint makes a stale port
// impossible rather than merely unlikely.
//
// If priming hasn't finished, or the file couldn't be written, the getter
// returns null and the wrapper omits `--settings`: the session runs normally,
// just without the shell's live view of it.

import { getEndpoint } from '@/lib/iyke/client';

let cached: string | null = null;
/** Distinguishes "primed, and the answer is null" from "never primed". */
let primed = false;
let inFlight: Promise<string | null> | null = null;

export function loadClaudeSettingsPath(): Promise<string | null> {
	if (primed) return Promise.resolve(cached);
	if (!inFlight) {
		inFlight = getEndpoint()
			.then((ep) => {
				cached = ep.hooks_settings_path ?? null;
				primed = true;
				return cached;
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
 */
export function getClaudeSettingsPathSync(): string | null {
	if (!primed) void loadClaudeSettingsPath();
	return cached;
}

/** Test seam — lets the argv tests assert both the primed and unprimed shapes. */
export function __setClaudeSettingsPathForTests(path: string | null): void {
	cached = path;
	primed = path !== null;
	inFlight = null;
}
