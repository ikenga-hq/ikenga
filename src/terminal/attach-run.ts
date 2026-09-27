// WP-69 (G-SEATS §4.4) — what a `run` seat's *Open in pane* / *Pop out* can
// show.
//
// WP-18b (ADR-023 D4/D5) retired tmux: a persistent run
// (`chi_run {persistent: true}`) is now a `chi-runner` spawned detached — its
// own process group, engine piped, no PTY — recorded by pid in
// `chi_cache.pid`. There is no terminal session to attach a client to, so
// every started run, persistent or one-off, is `headless` ("headless run —
// nothing to show"); a queued run or one outside the lookup is still not
// called headless (`RunAttachState`).
//
// The `tmux` state and the attach helpers below (`runAttachArgv`,
// `attachRunTerminal`, …) are no longer produced by `fetchRunAttachState`
// and are kept only so their callers compile unchanged; removing them with
// the menu paths that consume them is a tracked follow-up.

import { useQuery } from '@tanstack/react-query';

import { chiList } from '@/lib/tauri-cmd';
import { makeTerminalId, openTabPty, type TerminalTab, useTerminalStore } from './session-store';

/** `chi_list`'s own cap; the run is looked up among its engine's rows, so a
 *  run outside the newest rows reads `unknown` — never mislabelled
 *  headless. */
export const RUN_LOOKUP_LIMIT = 200;

/** How long a settled lookup stays fresh. */
const RUN_SESSION_STALE_MS = 15_000;
/** A queued run is re-read until it is spawned. */
const RUN_PENDING_POLL_MS = 3_000;

export interface RunRef {
	runId: string;
	/** Chi engine id — narrows the `chi_list` lookup. */
	engineId: string;
}

/**
 * What a run offers to attach to:
 * - `tmux`: retired with WP-18b — no longer produced (see the header);
 * - `headless`: a started run (a one-off, or a detached chi-runner) —
 *   nothing to show;
 * - `pending`: queued, not spawned yet;
 * - `unknown`: the run isn't among its engine's newest cached rows.
 */
export type RunAttachState =
	| { kind: 'tmux'; session: string }
	| { kind: 'headless' }
	| { kind: 'pending' }
	| { kind: 'unknown' };

export function runTerminalSessionKey(run: RunRef) {
	return ['chi', 'run-terminal-session', run.engineId, run.runId] as const;
}

/** Look the run up in `chi_cache` (see {@link RunAttachState}). */
export async function fetchRunAttachState(run: RunRef): Promise<RunAttachState> {
	const rows = await chiList(run.engineId, RUN_LOOKUP_LIMIT);
	const row = rows.find((r) => r.run_id === run.runId);
	if (!row) return { kind: 'unknown' };
	return row.status === 'queued' ? { kind: 'pending' } : { kind: 'headless' };
}

/** The attach terminal's tmux session, when the state has one. */
export function runSessionOf(state: RunAttachState | undefined): string | null {
	return state?.kind === 'tmux' ? state.session : null;
}

/** The argv of a tmux client on `session` (exact-name target). */
export function runAttachArgv(session: string): string[] {
	return ['tmux', 'attach-session', '-t', `=${session}`];
}

/** True when `cmd` is a tmux client attached to exactly `session`. */
export function isRunAttachCmd(cmd: readonly string[], session: string): boolean {
	return cmd[0] === 'tmux' && cmd.includes('attach-session') && cmd.includes(`=${session}`);
}

/** A running terminal tab already attached to `session`, if any. */
export function findRunAttachTerminal(tabs: readonly TerminalTab[], session: string): TerminalTab | null {
	return tabs.find((t) => t.status === 'running' && isRunAttachCmd(t.spec.cmd, session)) ?? null;
}

/**
 * Subscribe to a run seat's attach state (`undefined` while loading) and the
 * id of a running terminal already attached to its tmux session. `run: null`
 * (not a run seat) returns `{ state: undefined, terminalId: null }`. The one
 * read of the lookup: menus take `state` from here rather than touching the
 * cache during render.
 */
export function useRunAttachedTerminal(run: RunRef | null): {
	state: RunAttachState | undefined;
	terminalId: string | null;
} {
	const q = useQuery({
		queryKey: run ? runTerminalSessionKey(run) : ['chi', 'run-terminal-session', 'none'],
		queryFn: (): Promise<RunAttachState> =>
			run ? fetchRunAttachState(run) : Promise.resolve<RunAttachState>({ kind: 'unknown' }),
		enabled: run !== null,
		staleTime: RUN_SESSION_STALE_MS,
		refetchInterval: (query) => (query.state.data?.kind === 'pending' ? RUN_PENDING_POLL_MS : false),
	});
	const state = run ? q.data : undefined;
	const session = runSessionOf(state);
	const terminalId = useTerminalStore((s) =>
		session ? (findRunAttachTerminal(s.tabs, session)?.id ?? null) : null
	);
	return { state, terminalId };
}

/** One attach in flight per session, so a double click spawns one client. */
const inFlight = new Map<string, Promise<string>>();

/**
 * A terminal attached to the run's tmux `session`: the running one if there
 * is one, else a new tab spawned now (in-process, so Rust sees its mount).
 * Resolves to the terminal (tab) id; it is not mounted in a pane — the
 * caller opens it in a pane or pops it out. A failed spawn removes the tab.
 */
export function attachRunTerminal(opts: { session: string; cwd: string; title: string }): Promise<string> {
	const existing = findRunAttachTerminal(useTerminalStore.getState().tabs, opts.session);
	if (existing) return Promise.resolve(existing.id);
	const pending = inFlight.get(opts.session);
	if (pending) return pending;
	const p = (async () => {
		const id = makeTerminalId();
		useTerminalStore.getState().add({ cwd: opts.cwd, cmd: runAttachArgv(opts.session) }, opts.title, id);
		const tab = useTerminalStore.getState().tabs.find((t) => t.id === id);
		if (!tab) throw new Error('Could not create the attach terminal');
		try {
			await openTabPty(tab, { forceEphemeral: true });
		} catch (err) {
			useTerminalStore.getState().remove(id);
			throw err;
		}
		return id;
	})().finally(() => inFlight.delete(opts.session));
	inFlight.set(opts.session, p);
	return p;
}
