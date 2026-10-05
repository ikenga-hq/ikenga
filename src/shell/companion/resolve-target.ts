// Dispatch target resolution — spec §5.3, ADR-021.
//
//   resolveTarget(target) → { send(text, context) }
//
// Two implementations behind one shape:
//   • PTY inject — a `session` target with a live PTY: write the text into
//     that PTY (`ptyWrite`, the same command `POST /iyke/terminal/send` uses),
//     with the context PREPENDED as a comment line so a human reading the
//     terminal sees where the text came from.
//   • Chi runtime — a `session` target with no live PTY (a headless Chi run)
//     resumes the run (`chiResume`); a `new` / `persistent` target starts one
//     (`chiRun`). The context is APPENDED to the prompt.
//
// A `seat` target (G-SEATS §9.4, WP-66) labels itself from the TanStack
// Query cache of `seats_list` and decides its route at SEND time through
// `seats_resolve`: `pty` → the PTY inject above; `chi-resume` → `chiResume`
// (or `seatsQueue` while a turn is in flight, §4.5); `vacant` → path T (a
// resumed or new agent terminal with the text as its first prompt, then
// `seatsMove` with the claim) when the resolve granted a claim, otherwise
// path H (`seatsResume`, `fallback: 'fresh'`) — Round 47 erratum E-1.
//
// `send` is fire-and-forget by contract (ADR-021 checklist): its promise
// settles when the host accepted the text, never with a response to render.
// It resolves to `void` so no caller can grow a "response" slot from it.

import type { CompanionTarget } from '@/lib/shell/shell-store';
import { useShellStore } from '@/lib/shell/shell-store';
import { activeProjectCwd } from '@/lib/shell/active-project-cwd';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { PaneNode, PaneView } from '@/lib/panes/types';
import {
	cachedSeat,
	cachedSeats,
	ensureSeatsLiveSync,
	invalidateSeats,
	seatErrorOf,
	UI_SEAT_CLIENT,
} from '@/lib/queries/seats';
import {
	chiResume,
	chiRun,
	ptyWrite,
	type SeatActor,
	type SeatRoute,
	type SeatSession,
	type SeatView,
	seatsMove,
	seatsQueue,
	seatsResolve,
	seatsResume,
} from '@/lib/tauri-cmd';
import type { ModelRole } from '@/lib/model-catalog';
import {
	type AgentEngineKind,
	type AgentWrapOpts,
	buildAgentWrappedCmd,
} from '@/terminal/claude-wrap';
import { getPty } from '@/terminal/pty-registry';
import {
	makeTerminalId,
	openTabPty,
	type TerminalTab,
	useTerminalStore,
} from '@/terminal/session-store';
import {
	agentNotLiveText,
	queuedText,
	seatHeldText,
	seatTakenOverText,
	showSeatNotice,
	vacantDispatchText,
} from './seat-notice';
import { flushPendingClear } from './seat-pending';
import { aliasSessionNumber, seatSessionNumberText, sessionNumber } from './seat-sessions';

/** §5.3 `context` — where the dispatch came from. */
export interface DispatchContext {
	/** Active project root (or id when the project has no folder). */
	project?: string | null;
	/** The focused pane's current view (route or path). */
	focusedView?: string | null;
	/** Selected text / handed-off item, when there is one. */
	selection?: string | null;
}

export type ResolvedKind = 'pty' | 'chi-resume' | 'chi-run' | 'seat' | 'none';

export interface ResolvedTarget {
	kind: ResolvedKind;
	/** Engine a chi run starts on (chi-run), or the seat's engine (seat). */
	engineId?: string | null;
	/** The cached seat a `seat` target labels from (seat only; may be absent
	 *  while the roster loads). */
	seat?: SeatView;
	/** Why nothing resolves — the dispatch input's disabled `title`. */
	disabledReason?: string;
	send: (text: string, context?: DispatchContext) => Promise<void>;
}

export const NO_ENGINE_REASON = 'No engine installed — open Ngwa → Store';

function contextParts(ctx?: DispatchContext): string[] {
	if (!ctx) return [];
	const parts: string[] = [];
	if (ctx.project) parts.push(`project ${ctx.project}`);
	if (ctx.focusedView) parts.push(`view ${ctx.focusedView}`);
	if (ctx.selection) parts.push(`selection ${ctx.selection}`);
	return parts;
}

/** One-line comment prepended to a PTY write. Newlines are flattened so the
 *  comment can never become a second command. */
export function contextCommentLine(ctx?: DispatchContext): string | null {
	const parts = contextParts(ctx);
	if (parts.length === 0) return null;
	return `# ikenga · ${parts.join(' · ')}`.replace(/[\r\n]+/g, ' ');
}

/** Prompt with the context appended (chi_run / chi_resume). */
export function promptWithContext(text: string, ctx?: DispatchContext): string {
	const parts = contextParts(ctx);
	if (parts.length === 0) return text;
	return `${text}\n\nContext: ${parts.join('; ')}`;
}

/** Terminals that run an agent TUI rather than a shell (a `wrap` spec). A
 *  leading `# …` line typed into an agent's prompt is not a comment — Claude
 *  Code, for one, reads it as an instruction — so the context line is only
 *  prepended for plain shells. */
function isAgentTerminal(sessionId: string): boolean {
	const tab = useTerminalStore.getState().tabs.find((t) => t.id === sessionId);
	return Boolean(tab?.spec.wrap);
}

/** The core PTY id for a terminal session, if its PTY is live. */
function livePtyFor(sessionId: string): { ptyId: string; persistent: boolean } | null {
	const pty = getPty(sessionId);
	if (pty && !pty.exited) return { ptyId: pty.id, persistent: pty.mode === 'persistent' };
	const tab = useTerminalStore.getState().tabs.find((t) => t.id === sessionId);
	if (tab?.ptyId && tab.status === 'running') {
		return { ptyId: tab.ptyId, persistent: tab.mode === 'persistent' };
	}
	return null;
}

function engineFor(engineId: string | null): string | null {
	return engineId ?? useShellStore.getState().defaultEngineId ?? null;
}

function chiRunTarget(engineId: string | null, persistent: boolean): ResolvedTarget {
	const engine = engineFor(engineId);
	if (!engine) {
		return {
			kind: 'none',
			disabledReason: NO_ENGINE_REASON,
			send: async () => {
				throw new Error(NO_ENGINE_REASON);
			},
		};
	}
	return {
		kind: 'chi-run',
		engineId: engine,
		send: async (text, context) => {
			const cwd = useShellStore.getState().activeProject.root_path;
			const result = await chiRun({
				engineId: engine,
				prompt: promptWithContext(text, context),
				...(cwd ? { cwd } : {}),
				persistent,
			});
			if (result.status === 'failed' && result.error) throw new Error(result.error);
		},
	};
}

export function resolveTarget(target: CompanionTarget): ResolvedTarget {
	switch (target.kind) {
		case 'session': {
			const sessionId = target.session_id;
			if (livePtyFor(sessionId)) {
				return {
					kind: 'pty',
					send: async (text, context) => {
						// Re-resolve at send time: the PTY may have exited or respawned.
						const live = livePtyFor(sessionId);
						if (!live) throw new Error('That terminal is no longer running');
						const comment = isAgentTerminal(sessionId) ? null : contextCommentLine(context);
						const data = `${comment ? `${comment}\r` : ''}${text}\r`;
						if (live.persistent) {
							// Persistent PTYs live in the daemon; the Pty object routes
							// the write there (and falls back to `ptyWrite` itself).
							const pty = getPty(sessionId);
							if (pty) return pty.write(data);
						}
						await ptyWrite(live.ptyId, data);
					},
				};
			}
			return {
				kind: 'chi-resume',
				send: async (text, context) => {
					const result = await chiResume(sessionId, promptWithContext(text, context));
					if (result.status === 'failed' && result.error) throw new Error(result.error);
				},
			};
		}
		case 'new':
			return chiRunTarget(target.engine_id, false);
		case 'persistent':
			return chiRunTarget(target.engine_id, true);
		case 'seat':
			return seatTarget(target.seat_id);
	}
}

/** The engine a target's "new run" / "persistent run" keys start on: the
 *  target's own engine, a seat's engine (from the cache), or `null` (the
 *  default engine) for a session. */
export function targetEngineId(target: CompanionTarget): string | null {
	switch (target.kind) {
		case 'session':
			return null;
		case 'new':
		case 'persistent':
			return target.engine_id;
		case 'seat':
			return cachedSeat(target.seat_id)?.engine_id ?? null;
	}
}

// ─── Seat targets (G-SEATS §9.4, WP-66) ────────────────────────────────────

export const SEAT_GONE_REASON = 'That seat no longer exists';

/** Chi engine id → agent-wrap engine (§4.3 / §6.1): the `wrap_id` column of
 *  `iyke/seats.rs::ENGINE_CAPS`, the same test Rust uses to grant a path-T
 *  claim. Only these can take path T. */
const WRAP_ENGINE_FOR_CHI: Readonly<Record<string, AgentEngineKind>> = {
	'claude-code': 'claude',
	codex: 'codex',
	'antigravity-cli': 'antigravity',
	gemini: 'gemini',
};

/** The Chi engine can run in an agent terminal (§6.1 wrap id): *Fill* and
 *  *Resume* can occupy its seat without a first turn. */
export function engineRunsInTerminal(engineId: string): boolean {
	return WRAP_ENGINE_FOR_CHI[engineId] !== undefined;
}

/** A session ref's UI number handle: its terminal id or run id. */
function refOf(session: SeatSession | null | undefined): string | null {
	if (!session) return null;
	return session.kind === 'terminal' ? session.terminal_id : session.run_id;
}

function errorText(err: unknown): string {
	const seat = seatErrorOf(err);
	if (seat) return seat.message;
	return err instanceof Error ? err.message : String(err);
}

/** The UI's label for a session in the §6.3 texts (`<N>`): its UI number
 *  (`seat-sessions.ts`, D-09's "session 3"), replacing WP-66's 8-char id. */
export function seatSessionLabel(session: SeatSession | null | undefined): string {
	return seatSessionNumberText(session);
}

function seatTarget(seatId: string): ResolvedTarget {
	const projectId = useShellStore.getState().activeProject.id;
	const roster = projectId ? cachedSeats(projectId) : undefined;
	// A roster not loaded yet stays sendable: `send` decides at send time and
	// never reads the cache. Loading it is the subscribers' job (`useSeats`
	// in the dispatch bar / target chip) — nothing is fetched from here, as
	// this runs during render.
	if (roster !== undefined && !roster.some((s) => s.id === seatId)) {
		return {
			kind: 'none',
			disabledReason: SEAT_GONE_REASON,
			send: async () => {
				throw new Error(SEAT_GONE_REASON);
			},
		};
	}
	const seat = roster?.find((s) => s.id === seatId) ?? cachedSeat(seatId);
	return {
		kind: 'seat',
		engineId: seat?.engine_id ?? null,
		...(seat ? { seat } : {}),
		send: (text, context) => dispatchToSeat(seatId, text, context),
	};
}

type VacantRoute = Extract<SeatRoute, { route: 'vacant' }>;

/**
 * §9.4: resolve at send time, then send. `takeover` repeats the call after
 * the §5.2 refusal's *Take over*. A `seat_not_vacant` / `seat_resuming` race
 * goes back to the resolve once.
 */
export async function dispatchToSeat(
	seatId: string,
	text: string,
	context?: DispatchContext,
	opts: { takeover?: boolean; retried?: boolean } = {}
): Promise<void> {
	const actor: SeatActor = { client: UI_SEAT_CLIENT };
	// A Clear still in its 8 s Undo window commits first (§4.2): the send
	// starts from the vacant, history-less seat the rail shows, and the
	// Clear's timer can't later unseat the session this send starts.
	await flushPendingClear(seatId);
	// Fail-safe: never ask for a path-T claim for a seat known to be on an
	// engine with no terminal wrap (Rust grants none there either, E-1), so a
	// claim can't be left behind by a path T that can't run.
	const cachedEngine = cachedSeat(seatId)?.engine_id;
	const claimResume = cachedEngine === undefined || WRAP_ENGINE_FOR_CHI[cachedEngine] !== undefined;
	let route: SeatRoute;
	try {
		route = await seatsResolve({ seatId }, opts.takeover ? { ...actor, takeover: true } : actor, {
			claimResume,
		});
	} catch (err) {
		throw refusal(err, seatId, text, context);
	}
	const seat = route.seat;
	try {
		switch (route.route) {
			case 'pty': {
				// The existing inject (the `session` branch, context-line rule included).
				const inject = resolveTarget({ kind: 'session', session_id: route.terminal_id });
				if (inject.kind !== 'pty') {
					throw new Error(`The terminal in @${seat.name} is no longer running`);
				}
				await inject.send(text, context);
				return;
			}
			case 'chi-resume': {
				const prompt = promptWithContext(text, context);
				if (route.busy) {
					// §4.5: never `chi_resume` over a turn in flight. E-4: be
					// subscribed before queueing, so a dropped text is never silent.
					ensureSeatsLiveSync();
					await seatsQueue(seat.id, prompt, actor);
					showSeatNotice(queuedText(seat.name));
					return;
				}
				const result = await chiResume(route.run_id, prompt);
				if (result.status === 'failed' && result.error) throw new Error(result.error);
				return;
			}
			case 'vacant':
				// E-1: a claim is granted only where path T is possible.
				if (route.claim) await sendPathT(route, route.claim, text, context);
				else await sendPathH(route, text, context);
				return;
		}
	} catch (err) {
		const code = seatErrorOf(err)?.code;
		if ((code === 'seat_not_vacant' || code === 'seat_resuming') && !opts.retried) {
			return dispatchToSeat(seatId, text, context, { retried: true });
		}
		// A hold acquired between the resolve and the write refuses the write
		// the same way (§5.2 / §5.3), with the same notice and *Take over*.
		// Everything else is typed by `refusal` too (exact texts).
		throw refusal(err, seatId, text, context);
	} finally {
		void invalidateSeats(seat.project_id);
	}
}

/** The Error a failed `seats_resolve` rejects `send` with — and, for a
 *  hold (§5.2) or a takeover notice (§5.3), the notice that explains it. */
function refusal(err: unknown, seatId: string, text: string, context?: DispatchContext): Error {
	const seatErr = seatErrorOf(err);
	if (!seatErr) return err instanceof Error ? err : new Error(String(err));
	const name = cachedSeat(seatId)?.name ?? 'This seat';
	switch (seatErr.code) {
		case 'seat_held': {
			const d = seatErr.details as { client?: string; since?: number } | undefined;
			const message = seatHeldText(name, d?.client ?? 'another client', d?.since ?? Date.now());
			showSeatNotice(message, {
				variant: 'error',
				action: {
					label: 'Take over',
					run: () => takeOverAndSend(seatId, text, context),
				},
			});
			return new Error(message);
		}
		case 'seat_taken_over': {
			const d = seatErr.details as { by?: string; at?: number } | undefined;
			const message = seatTakenOverText(name, d?.by ?? 'another client', d?.at ?? Date.now());
			showSeatNotice(message, { variant: 'error' });
			return new Error(message);
		}
		case 'agent_not_live':
			return new Error(agentNotLiveText(name));
		case 'seat_resuming':
			// Another dispatch holds the 30 s path-T claim (§4.1, P-12).
			return new Error(seatResumingText(cachedSeat(seatId)?.name ?? null));
		case 'seat_not_found':
			return new Error(SEAT_GONE_REASON);
		default:
			return new Error(seatErr.message);
	}
}

/** A `seat_resuming` refusal: the seat is mid path T for another dispatch. */
export function seatResumingText(name: string | null): string {
	return `${name ? `@${name}` : 'This seat'} is being resumed — try again shortly`;
}

/** *Take over*: repeat the send with `takeover: true`; on success the
 *  dispatch input (still holding the refused text) is cleared. */
async function takeOverAndSend(
	seatId: string,
	text: string,
	context?: DispatchContext
): Promise<void> {
	try {
		await dispatchToSeat(seatId, text, context, { takeover: true });
		const { useCompanionStore } = await import('./companion-store');
		const companion = useCompanionStore.getState();
		if (companion.draft.trim() === text.trim()) companion.setDraft('');
	} catch (err) {
		showSeatNotice(errorText(err), { variant: 'error' });
	}
}

/**
 * §4.1 path T: spawn an agent terminal on the seat's engine — resumed when
 * the seat is resumable, new otherwise — with the text as its initial
 * positional prompt, then bind it with `seatsMove` and the claim.
 */
async function sendPathT(
	route: VacantRoute,
	claim: string,
	text: string,
	context?: DispatchContext
) {
	const seat = route.seat;
	const engine = WRAP_ENGINE_FOR_CHI[seat.engine_id];
	// Unreachable under E-1 (Rust grants a claim only for a wrap engine); the
	// claim then lapses by itself after 30 s.
	if (!engine)
		throw new Error(`@${seat.name}'s engine (${seat.engine_id}) can't run in a terminal`);
	const previous = seat.session;
	const resumeId = route.resume.resumable ? (previous?.external_id ?? null) : null;
	const cwd = previous?.cwd ?? useShellStore.getState().activeProject.root_path ?? null;
	const terminalId = await spawnSeatTerminal({
		engine,
		cwd,
		prompt: promptWithContext(text, context),
		resumeSessionId: resumeId,
		title: `@${seat.name}`,
		// A resumed conversation keeps its number (D-09: "resumed session 2").
		numberAs: resumeId ? refOf(previous) : null,
	});
	// The text is in the terminal now: from here a failure is reported, not thrown.
	try {
		await seatsMove(
			{ kind: 'terminal', terminalId, engineId: seat.engine_id, cwd, externalId: resumeId },
			seat.id,
			{ client: UI_SEAT_CLIENT },
			{ claim }
		);
	} catch (err) {
		showSeatNotice(
			`Sent to a new ${seat.engine_id} terminal, but it couldn't be seated — @${seat.name} is still vacant (${errorText(err)})`,
			{ variant: 'error' }
		);
		return;
	}
	const reason = route.resume.resumable ? undefined : route.resume.reason;
	showSeatNotice(
		vacantDispatchText(
			seat.name,
			seat.engine_id,
			resumeId
				? { outcome: 'resumed', session: seatSessionLabel(previous) }
				: { outcome: 'started-fresh', reason, session: String(sessionNumber(terminalId)) }
		)
	);
}

/** §4.1 path H: `seatsResume` with `fallback: 'fresh'` binds the run itself. */
async function sendPathH(route: VacantRoute, text: string, context?: DispatchContext) {
	const seat = route.seat;
	const result = await seatsResume(
		seat.id,
		promptWithContext(text, context),
		{ client: UI_SEAT_CLIENT },
		{ fallback: 'fresh' }
	);
	const resumedRef = result.outcome === 'resumed' ? refOf(result.previous) : null;
	if (resumedRef) aliasSessionNumber(result.run_id, resumedRef);
	showSeatNotice(
		vacantDispatchText(
			seat.name,
			seat.engine_id,
			result.outcome === 'resumed'
				? { outcome: 'resumed', session: seatSessionLabel(result.previous) }
				: {
						outcome: 'started-fresh',
						reason: result.reason,
						session: String(sessionNumber(result.run_id)),
					}
		)
	);
}

/** The outcome of an explicit *Resume* / *Fill* (WP-67, §9.3). */
export interface OccupyResult {
	seat: SeatView;
	/** The new agent terminal now in the seat. */
	terminalId: string;
	/** Whether it resumed a past conversation or started a new one. */
	outcome: 'resumed' | 'filled';
	/** The session it resumed, for the toast's `session N`. */
	previous: SeatSession | null;
}

/**
 * §9.3 *Resume session N* / *Fill with a new session* (and the create form's
 * *new session* / *resume a past session*): `seats_resolve({claimResume})` →
 * today's agent-terminal spawn with **no prompt** → `seats_move({claim})`.
 * The same path T a dispatch takes, minus the first turn.
 *
 * - `resume` resumes the seat's own last session, or `from` (the create
 *   form's past session — the move then takes it from its old seat, DEC-69c).
 *   An explicit resume never falls back to a fresh start (§6.2).
 * - A seat with no path T (a run-kind seat or a runs-only engine, E-1) has
 *   no interactive resume: it resumes on its first dispatch (§7.2
 *   `needs_prompt`), which the thrown message says. A run-kind seat on a
 *   wrap engine can still be *filled*: with no claim to carry, the new
 *   terminal binds by a plain move (§4.3, always legal). A runs-only engine
 *   can't hold a terminal at all, so it fills on its first dispatch.
 */
export async function occupyVacantSeat(
	seatId: string,
	mode: 'resume' | 'fill',
	opts: { from?: SeatSession | null } = {}
): Promise<OccupyResult> {
	const actor: SeatActor = { client: UI_SEAT_CLIENT };
	let route: SeatRoute;
	try {
		route = await seatsResolve({ seatId }, actor, { claimResume: true });
	} catch (err) {
		throw new Error(errorText(err));
	}
	const seat = route.seat;
	try {
		if (route.route !== 'vacant') throw new Error(`@${seat.name} is not vacant any more`);
		const engine = WRAP_ENGINE_FOR_CHI[seat.engine_id];
		const from = mode === 'resume' ? (opts.from ?? seat.session) : null;
		// E-1: no claim for a run-kind seat (or a runs-only engine). A *Fill*
		// on a wrap engine still works — a move is always legal (§4.3) — but
		// resuming a run is path H, which needs the first turn: dispatch it.
		const headless = !engine || (!route.claim && mode === 'resume');
		if (headless) {
			throw new Error(
				`@${seat.name} runs headless — dispatch an instruction to it and it will ${
					mode === 'resume' ? 'resume' : 'fill'
				}, then send`
			);
		}
		let resumeId: string | null = null;
		if (mode === 'resume') {
			if (!opts.from && !route.resume.resumable) {
				throw new Error(`@${seat.name} can't resume — fill it with a new session instead`);
			}
			resumeId = from?.external_id ?? null;
			if (!resumeId) throw new Error(`@${seat.name}'s last session left no resume id`);
		}
		const cwd = from?.cwd ?? useShellStore.getState().activeProject.root_path ?? null;
		const terminalId = await spawnSeatTerminal({
			engine,
			cwd,
			prompt: null,
			resumeSessionId: resumeId,
			title: `@${seat.name}`,
			// A resumed conversation keeps its number (D-09: "resumed session 2").
			numberAs: resumeId ? refOf(from) : null,
		});
		const moved = await seatsMove(
			{ kind: 'terminal', terminalId, engineId: seat.engine_id, cwd, externalId: resumeId },
			seat.id,
			actor,
			route.claim ? { claim: route.claim } : undefined
		);
		return {
			seat: moved.seat,
			terminalId,
			outcome: resumeId ? 'resumed' : 'filled',
			previous: from,
		};
	} catch (err) {
		throw new Error(errorText(err));
	} finally {
		void invalidateSeats(seat.project_id);
	}
}

/**
 * Today's agent-terminal spawn, for path T: a wrap tab (so the hooks
 * listener, liveness and the per-terminal `--settings` all apply), spawned
 * at once — `seats_move` binds only a terminal Rust already knows — and not
 * mounted in a pane (the seat's mount is a readout, §2.3).
 *
 * The resume id rides the tab's `claudeSessionId`, which is what
 * `buildSpawnOpts` resumes from, set WITHOUT marking the agent live (only
 * `SessionStart` does that, so the runner's liveness guard stays
 * fail-closed). The prompt is dropped from the tab's spec once the PTY has
 * spawned, so a later respawn never sends the text a second time.
 *
 * The PTY is forced in-process (`forceEphemeral`): a daemon-backed terminal
 * is invisible to Rust and can't be seated (P-10). A spawn that fails
 * removes the tab, so no half-made `--resume` terminal is left to respawn.
 */
/** The wrap a seat terminal launches with. A Claude seat is an everyday
 *  `pane` unless a role is given (WP-11), so with no model it starts on the
 *  catalog's pane model. */
export function seatWrapOpts(opts: {
	engine: AgentEngineKind;
	prompt: string | null;
	cwd: string;
	role?: ModelRole;
}): AgentWrapOpts {
	return { engine: opts.engine, prompt: opts.prompt, cwd: opts.cwd, role: opts.role ?? 'pane' };
}

async function spawnSeatTerminal(opts: {
	engine: AgentEngineKind;
	cwd: string | null;
	/** The first turn; `null` for Fill / Resume with nothing typed (§9.3). */
	prompt: string | null;
	resumeSessionId: string | null;
	title: string;
	/** The session ref whose UI number the new terminal takes (a resume). */
	numberAs?: string | null;
	/** WP-11 launch role for a Claude seat; defaults to an everyday `pane`. */
	role?: ModelRole;
}): Promise<string> {
	const id = makeTerminalId();
	// Before the tab reaches the store, so no render numbers it first.
	if (opts.numberAs) aliasSessionNumber(id, opts.numberAs);
	const cwd = opts.cwd ?? activeProjectCwd();
	const wrap = seatWrapOpts({ engine: opts.engine, prompt: opts.prompt, cwd, role: opts.role });
	const cmd = buildAgentWrappedCmd({
		...wrap,
		terminalId: id,
		resumeSessionId: opts.resumeSessionId,
	});
	useTerminalStore.getState().add({ cwd, cmd, wrap }, opts.title, id);
	const withResume = (tab: TerminalTab): TerminalTab =>
		tab.id === id ? { ...tab, claudeSessionId: opts.resumeSessionId } : tab;
	useTerminalStore.setState((s) => ({ tabs: s.tabs.map(withResume) }));
	const tab = useTerminalStore.getState().tabs.find((t) => t.id === id);
	if (!tab) throw new Error('Could not create the seat terminal');
	try {
		await openTabPty(tab, { forceEphemeral: true });
	} catch (err) {
		useTerminalStore.getState().remove(id);
		throw err;
	} finally {
		useTerminalStore.setState((s) => ({
			tabs: s.tabs.map((t) =>
				t.id === id && t.spec.wrap
					? { ...t, spec: { ...t.spec, wrap: { ...t.spec.wrap, prompt: null } } }
					: t
			),
		}));
	}
	return id;
}

function findLeaf(node: PaneNode, id: string): Extract<PaneNode, { type: 'leaf' }> | null {
	if (node.type === 'leaf') return node.id === id ? node : null;
	for (const child of node.children) {
		const found = findLeaf(child, id);
		if (found) return found;
	}
	return null;
}

function describeView(view: PaneView | undefined): string | null {
	if (!view) return null;
	switch (view.kind) {
		case 'route':
			return view.path || '/';
		case 'terminal':
			return `terminal ${view.sessionId}`;
		case 'scratchpad':
			return `scratchpad ${view.name}`;
		default:
			return view.path;
	}
}

/** The §5.3 context for a dispatch made right now. */
export function currentDispatchContext(selection?: string | null): DispatchContext {
	const project = useShellStore.getState().activeProject;
	const { root, focusedId } = usePaneStore.getState();
	const leaf = findLeaf(root, focusedId);
	return {
		project: project.root_path ?? (project.id !== 'default' ? project.id : null),
		focusedView: describeView(leaf?.tabs[leaf.activeTabIdx]),
		selection: selection ?? null,
	};
}
