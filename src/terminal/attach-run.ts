// WP-69 (G-SEATS §4.4) — a terminal attached to a persistent Chi run's tmux
// session, so *Open in pane* and *Pop out* have something to show for a
// `run` seat.
//
// A persistent run (`chi_run {persistent: true}`) runs `chi-runner` inside a
// detached tmux session named after the run id, and records that name in
// `chi_cache.terminal_session_id` (`src-tauri/src/terminal/multiplexer.rs`).
// Attaching is what `iyke chi attach <run_id>` does: a tmux client on that
// session. Here the client is an ordinary terminal tab whose argv is
// `tmux attach-session -t =<session>` (`=` = exact-name match, so `run-1`
// never attaches to `run-12`). Rust recognises it as the run's mount — a
// running terminal whose argv carries `tmux` and `=<session>`
// (`iyke/seats.rs::tmux_mount`) — so the seat's `mount` follows it into a
// pane or Window 2.
//
// The PTY is forced in-process (`forceEphemeral`), like a seat's own
// terminal: a daemon-backed PTY is invisible to Rust (P-10), so the mount
// could never be derived. Closing the tab only detaches the tmux client; the
// run keeps going. A one-off run (no `terminal_session_id`) has nothing to
// attach to, and callers say "headless run — nothing to show"; a queued run
// or one outside the lookup is not called headless (`RunAttachState`).

import { useQuery } from '@tanstack/react-query';

import { chiList } from '@/lib/tauri-cmd';
import { makeTerminalId, openTabPty, type TerminalTab, useTerminalStore } from './session-store';

/** `chi_list`'s own cap; the run is looked up among its engine's rows.
 *  There is no by-run-id read that carries `terminal_session_id` today
 *  (`chi_status` returns only status / output), so a run outside the newest
 *  rows reads `unknown` — never mislabelled headless. */
export const RUN_LOOKUP_LIMIT = 200;

/** How long a settled lookup stays fresh. The name never changes once
 *  written. */
const RUN_SESSION_STALE_MS = 15_000;
/** A queued run gets its tmux session only once it is spawned: re-read. */
const RUN_PENDING_POLL_MS = 3_000;

export interface RunRef {
	runId: string;
	/** Chi engine id — narrows the `chi_list` lookup. */
	engineId: string;
}

/**
 * What a run offers to attach to:
 * - `tmux`: a persistent run's tmux session (`chi_cache.terminal_session_id`);
 * - `headless`: a started one-off run — nothing to show;
 * - `pending`: queued, not spawned yet, so whether it gets a session isn't
 *   known;
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
	const session = row.terminal_session_id?.trim();
	if (session) return { kind: 'tmux', session };
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
