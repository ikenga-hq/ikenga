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
