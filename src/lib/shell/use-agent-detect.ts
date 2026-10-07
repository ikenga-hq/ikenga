// Per-engine reveal-as-found PATH scan for the onboarding agent step.
//
// Each engine resolves on its own promise so the UI flips its status pill
// independently — the slowest probe never blocks the fastest. The hook
// caches results per-mount; consumers re-trigger a scan via `refresh()`.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { agentUnavailableText } from '@/lib/agent-unavailable';
import { detectAgent, type DetectedAgent } from '@/lib/tauri-cmd';

/** `unavailable` = the probe ran but couldn't check (WSL couldn't be asked,
 *  D-10): `agent` names it, `error` carries "WSL unavailable — <reason>". It is
 *  neither detected (not runnable) nor missing. */
export type AgentDetectStatus = 'pending' | 'detected' | 'missing' | 'unknown' | 'unavailable';

export interface AgentDetectEntry {
	status: AgentDetectStatus;
	agent?: DetectedAgent;
	error?: string;
}

export type AgentDetectMap = Record<string, AgentDetectEntry>;

export interface UseAgentDetectResult {
	results: AgentDetectMap;
	refresh: () => void;
}

export function pendingMap(ids: readonly string[]): AgentDetectMap {
	const next: AgentDetectMap = {};
	for (const id of ids) next[id] = { status: 'pending' };
	return next;
}

/** Result shape for a single resolved probe — exported so consumers can
 *  drive the same state transitions in tests / smoke harnesses without
 *  mounting React. */
export function entryFromProbe(
	agent: DetectedAgent | null | undefined,
	error?: unknown
): AgentDetectEntry {
	if (error != null) {
		return { status: 'unknown', error: String((error as Error)?.message ?? error) };
	}
	if (!agent) return { status: 'missing' };
	const unavailable = agentUnavailableText(agent);
	if (unavailable) return { status: 'unavailable', agent, error: unavailable };
	return { status: 'detected', agent };
}

/** Pure reducer for the run-token guard: returns the next map if `token`
 *  still matches `currentToken`, else returns the previous map untouched.
 *  Exposed so the run-token semantics can be unit-tested without React. */
export function applyProbeResult(
	prev: AgentDetectMap,
	id: string,
	entry: AgentDetectEntry,
	token: number,
	currentToken: number
): AgentDetectMap {
	if (token !== currentToken) return prev;
	return { ...prev, [id]: entry };
}

/** Pure helper to compute if all engines in `engineIds` are missing.
 *  Returns false if any engine is detected, pending, unknown (error) or
 *  unavailable (couldn't be checked) — only a confirmed miss counts. */
export function computeAllMissing(
	results: AgentDetectMap,
	engineIds: readonly string[]
): boolean {
	// An id with no entry yet counts as missing, as it always has.
	return engineIds.every((id) => (results[id]?.status ?? 'missing') === 'missing');
}

export function useAgentDetect(engineIds: readonly string[]): UseAgentDetectResult {
	const idsKey = useMemo(() => engineIds.join('|'), [engineIds]);
	const [results, setResults] = useState<AgentDetectMap>(() => pendingMap(engineIds));
	const runRef = useRef(0);

	// biome-ignore lint/correctness/useExhaustiveDependencies: `idsKey` is a stable identity for the engineIds array; re-running only when the joined key changes is the desired behaviour.
	const scan = useCallback(() => {
		const token = ++runRef.current;
		setResults(pendingMap(engineIds));
		for (const id of engineIds) {
			detectAgent(id).then(
				(agent) => {
					setResults((prev) =>
						applyProbeResult(prev, id, entryFromProbe(agent), token, runRef.current)
					);
				},
				(err: unknown) => {
					setResults((prev) =>
						applyProbeResult(prev, id, entryFromProbe(null, err), token, runRef.current)
					);
				}
			);
		}
	}, [idsKey]);

	useEffect(() => {
		scan();
	}, [scan]);

	return { results, refresh: scan };
}
