// D-10: an agent detection couldn't check (WSL couldn't be asked) is neither
// installed nor missing. These helpers give every surface one wording and one
// runnable test, so none says "not installed" / "Not on PATH" for it and none
// offers it as a target. Kept out of `tauri-cmd.ts` so modules that mock that
// file still get the real helpers.

import type { DetectedAgent } from '@/lib/tauri-cmd';

/** "WSL unavailable — <reason>" for an agent detection couldn't check, else
 *  null. */
export function agentUnavailableText(
	agent: Pick<DetectedAgent, 'unavailable'> | null | undefined
): string | null {
	const u = agent?.unavailable;
	if (!u) return null;
	const what = u.kind === 'wsl' ? 'WSL unavailable' : `${u.kind} unavailable`;
	const reason = u.reason?.trim();
	return reason ? `${what} — ${reason}` : what;
}

/** True when `agent` was found and can be run — not merely named because
 *  WSL couldn't be asked. */
export function isRunnableAgent(agent: DetectedAgent | null | undefined): agent is DetectedAgent {
	return !!agent && !agent.unavailable;
}

/** What onboarding stored about the chosen engine (its step payload). */
export interface EnginePayloadFacts {
	executablePath?: string;
	version?: string | null;
	authed?: boolean | null;
}

/** The auth / path / version a settings surface should show for the selected
 *  engine. Live detection wins; the onboarding payload fills gaps only when
 *  detection actually checked the engine. For an unchecked engine (WSL
 *  couldn't be asked) every fact is unknown — the payload is stale and the
 *  live `executable_path` is only a placeholder name. */
export function engineFacts(
	live: DetectedAgent | null | undefined,
	payload: EnginePayloadFacts | null | undefined
): { authed: boolean | null; execPath: string | undefined; version: string | null | undefined } {
	if (live?.unavailable) return { authed: null, execPath: undefined, version: null };
	return {
		authed: live?.authed ?? payload?.authed ?? null,
		execPath: live?.executable_path ?? payload?.executablePath,
		version: live?.version ?? payload?.version,
	};
}
