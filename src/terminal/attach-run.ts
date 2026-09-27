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
// attach to, and callers say "headless run — nothing to show".

import { useQuery } from '@tanstack/react-query';

import { queryClient } from '@/lib/query-client';
import { chiList } from '@/lib/tauri-cmd';
import { makeTerminalId, openTabPty, type TerminalTab, useTerminalStore } from './session-store';

/** `chi_list`'s own cap; the run is looked up among its engine's rows. */
export const RUN_LOOKUP_LIMIT = 200;

/** How long a run's tmux-session lookup stays fresh. The name never changes
 *  once written, but a queued run gets it only once it is spawned. */
const RUN_SESSION_STALE_MS = 15_000;

export interface RunRef {
	runId: string;
	/** Chi engine id — narrows the `chi_list` lookup. */
	engineId: string;
}

export function runTerminalSessionKey(run: RunRef) {
	return ['chi', 'run-terminal-session', run.engineId, run.runId] as const;
}

/** `chi_cache.terminal_session_id` for a run: the tmux session name, or
 *  `null` for a one-off (non-persistent) run or a run no longer cached. */
export async function fetchRunTerminalSession(run: RunRef): Promise<string | null> {
	const rows = await chiList(run.engineId, RUN_LOOKUP_LIMIT);
	const row = rows.find((r) => r.run_id === run.runId);
	const session = row?.terminal_session_id?.trim();
	return session ? session : null;
}

function runSessionQuery(run: RunRef) {
	return {
		queryKey: runTerminalSessionKey(run),
		queryFn: () => fetchRunTerminalSession(run),
		staleTime: RUN_SESSION_STALE_MS,
	};
}

/**
 * The cached tmux session of `run`, read synchronously: a name, `null` (a
 * one-off run), or `undefined` while it isn't known yet — in which case a
 * fetch is started, and a component subscribed through
 * {@link useRunAttachedTerminal} re-renders when it lands.
 */
export function cachedRunTerminalSession(run: RunRef): string | null | undefined {
	const data = queryClient.getQueryData<string | null>(runTerminalSessionKey(run));
	if (data === undefined) void queryClient.prefetchQuery(runSessionQuery(run)).catch(() => {});
	return data;
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
 * Subscribe to a run seat's attach state: its tmux session (`undefined`
 * while loading) and the id of a running terminal already attached to it.
 * `run: null` (not a run seat) returns `{ session: null, terminalId: null }`.
 */
export function useRunAttachedTerminal(run: RunRef | null): {
	session: string | null | undefined;
	terminalId: string | null;
} {
	const q = useQuery({
		queryKey: run ? runTerminalSessionKey(run) : ['chi', 'run-terminal-session', 'none'],
		queryFn: () => (run ? fetchRunTerminalSession(run) : Promise.resolve(null)),
		enabled: run !== null,
		staleTime: RUN_SESSION_STALE_MS,
	});
	const session = run ? q.data : null;
	const terminalId = useTerminalStore((s) =>
		session ? (findRunAttachTerminal(s.tabs, session)?.id ?? null) : null
	);
	return { session, terminalId };
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
