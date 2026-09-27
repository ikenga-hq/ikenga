// WP-67 — pure helpers for the seat rail (D-09 `seats-companion.html`,
// G-SEATS §1.2, §1.6, §5.2, §6.2, §6a, §7.3). Nothing here touches a store
// or the host: every function maps a `SeatView` (or a form) to the words and
// signals the rail shows, so the texts are testable on their own.
//
// ADR-021: a seat shows STATE (session, live/idle/run/vacant, where it is
// mounted, what is pending) and ADDRESSES (`@name`, the iyke form). Nothing
// here formats model output.

import type { EngineResume, NotResumableReason, SeatStatus, SeatView } from '@/lib/tauri-cmd';

/** D-09 revision 2: a figure an engine hasn't reported renders "—" with this tooltip. */
export const UNREPORTED = 'not reported by this engine yet';

/** G-SEATS §4.2: Clear and Remove are undone by a client-side delay this long. */
export const SEAT_UNDO_MS = 8_000;

// ─── Names (§1.2) ───────────────────────────────────────────────────────────

export const SEAT_NAME_MAX = 32;
const SEAT_NAME_RE = /^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$/;

export type SeatNameCheck =
	| { ok: true; empty?: false; message: string }
	| { ok: false; empty: boolean; message: string };

/**
 * Live validation, in the order the locked form reports it: charset, the
 * hyphen rule, length, then uniqueness ("`<name>` is already a seat").
 * `except` is the seat being renamed (its own name is not a clash);
 * `removing` are seats inside their 8 s Remove window — hidden from the
 * rail, but their names stay taken until the host call lands.
 */
export function checkSeatName(
	raw: string,
	taken: readonly string[],
	opts: { except?: string; project?: string; removing?: readonly string[] } = {}
): SeatNameCheck {
	const name = raw.trim();
	if (!name) return { ok: false, empty: true, message: '' };
	if (!/^[a-z0-9-]+$/.test(name)) return { ok: false, empty: false, message: 'Use a–z, 0–9 and - only' };
	if (name.startsWith('-') || name.endsWith('-')) {
		return { ok: false, empty: false, message: 'A name can’t start or end with -' };
	}
	if (name.length > SEAT_NAME_MAX) {
		return { ok: false, empty: false, message: `At most ${SEAT_NAME_MAX} characters` };
	}
	if (taken.some((t) => t === name && t !== opts.except)) {
		return { ok: false, empty: false, message: `${name} is already a seat` };
	}
	if (opts.removing?.includes(name) && name !== opts.except) {
		return { ok: false, empty: false, message: `${name} is being removed — Undo it or wait 8 s` };
	}
	// The grammar above is §1.2's, restated; keep the regex as the arbiter.
	if (!SEAT_NAME_RE.test(name)) return { ok: false, empty: false, message: 'Use a–z, 0–9 and - only' };
	return { ok: true, message: `@${name} is free${opts.project ? ` in ${opts.project}` : ''}` };
}

// ─── Addresses (§1.3, §6a) ──────────────────────────────────────────────────

/** `@name` — the UI short form (chip, rail, *Copy address*). */
export function atName(name: string): string {
	return `@${name}`;
}

/** The canonical scratchpad scope, `seat:<project>/<name>` (§6a: never `seat:<name>`). */
export function seatScope(projectId: string, name: string): string {
	return `seat:${projectId}/${name}`;
}

/** Two-letter monogram for the 36 px rest strip. */
export function seatMonogram(name: string): string {
	return name.slice(0, 2);
}

// ─── Engines (§6.1) ─────────────────────────────────────────────────────────

/** The short engine word the chip and rows use (`claude-code` → `claude`). */
export function engineShort(engineId: string): string {
	return engineId === 'claude-code' ? 'claude' : engineId;
}

/** Terminal wrap engine → Chi engine id (§4.3). `gemini` has no Chi id. */
export function chiEngineForWrap(wrap: string | null | undefined): string | null {
	switch (wrap) {
		case 'claude':
			return 'claude-code';
		case 'codex':
			return 'codex';
		case 'antigravity':
			return 'antigravity-cli';
		default:
			return null;
	}
}

/** §6.2 / P-5: the flag a seat carries at all times, or null. */
export function engineResumeFlag(resume: EngineResume | null | undefined): string | null {
	if (resume === 'process-local') return 'not resumable after restart';
	if (resume === 'none') return 'can’t resume sessions';
	return null;
}

/** Why *Resume* is disabled on a vacant seat (§6.2), in the rail's words. */
export function notResumableText(reason: NotResumableReason): string {
	switch (reason) {
		case 'no_session':
			return 'no past session — it was cleared';
		case 'process_local':
			return 'not resumable after restart';
		case 'no_resume_support':
			return 'this engine can’t resume sessions';
		case 'no_resume_id':
			return 'no resume id was recorded for its last session';
		case 'run_missing':
			return 'its last run no longer exists';
		case 'engine_unavailable':
			return 'its engine is not installed — Ngwa → Store';
	}
}

// ─── State (§2.1) ───────────────────────────────────────────────────────────

/** The CSS colour token for a state dot; `vacant` is drawn as a ring. */
export function stateDotColor(status: SeatStatus): string {
	switch (status) {
		case 'live':
			return 'var(--live)';
		case 'run':
			return 'var(--ember)';
		case 'idle':
			return 'var(--fg-faint)';
		case 'vacant':
			return 'transparent';
	}
}

/** The session reference the panels scope to (§9.1): a terminal id or a run id. */
export function seatSessionRef(seat: Pick<SeatView, 'session'>): string | null {
	const s = seat.session;
	if (!s) return null;
	return s.kind === 'terminal' ? s.terminal_id : s.run_id;
}

/**
 * The row's second word group — what sits in the seat. `sessionName` is the
 * UI's own numbering (`session 3`), passed in so this stays pure.
 */
export function seatWhoLine(seat: SeatView, sessionName: string | null): string {
	if (seat.status === 'vacant') {
		if (!seat.session) return 'vacant · no history';
		return `vacant · ${sessionName ?? 'last session'} ended`;
	}
	if (seat.status === 'run') return `run · ${sessionName ?? 'a run'}`;
	const idle = seat.status === 'idle' ? ' · idle' : '';
	return `${seat.engine_id} · ${sessionName ?? 'session'}${idle}`;
}

/** The chip's trailing half: `· claude · session 3`, or the vacant promise. */
export function seatChipRest(seat: SeatView, sessionName: string | null): string {
	if (seat.status === 'vacant') {
		if (seat.session && seat.resume.resumable) return ` · vacant · resumes ${sessionName ?? 'its session'}`;
		return ' · vacant · fills on send';
	}
	return ` · ${engineShort(seat.engine_id)} · ${sessionName ?? 'session'}`;
}

/** What ↵ will do for a seat target — the dispatch hint row (D-09 `d9Hint`). */
export function seatSendHint(seat: SeatView, sessionName: string | null): string {
	if (seat.status === 'vacant') {
		if (seat.session && seat.resume.resumable) return `resume ${sessionName ?? 'its session'}, then send`;
		return `fill @${seat.name}, then send`;
	}
	if (seat.status === 'run') return `resume the run in @${seat.name}`;
	return `send to @${seat.name}`;
}

// ─── Holds (§5.2) ───────────────────────────────────────────────────────────

/** "held by X since T" — only while unexpired. `formatTime` is injected. */
export function holdText(
	hold: SeatView['hold'],
	formatTime: (ms: number) => string,
	now = Date.now()
): string | null {
	if (!hold || hold.expires_at <= now) return null;
	return `held by ${hold.client} since ${formatTime(hold.since)}`;
}

/** Another client holds the seat (so *Take over* shows, §5.5). */
export function heldByOther(hold: SeatView['hold'], client: string, now = Date.now()): boolean {
	return Boolean(hold && hold.expires_at > now && hold.client !== client);
}

// ─── Pad (§1.6) ─────────────────────────────────────────────────────────────

export function padText(seat: Pick<SeatView, 'pad' | 'inbox_count' | 'status'>): string {
	const n = seat.pad.count;
	let t = `${n} ${n === 1 ? 'entry' : 'entries'}`;
	if (seat.pad.latest) t += ` · “${seat.pad.latest.name}”`;
	if (seat.inbox_count > 0 && seat.status !== 'vacant') t += ` · inbox ${seat.inbox_count}`;
	return t;
}

// ─── Figures (D-09 revision 2) ──────────────────────────────────────────────

/** `38120` → `38k`; `null`/`undefined` → `null` (not reported). */
export function ctxK(tokens: number | null | undefined): string | null {
	if (tokens == null || !Number.isFinite(tokens) || tokens <= 0) return null;
	return tokens >= 1000 ? `${Math.round(tokens / 1000)}k` : String(Math.round(tokens));
}

/** `1.4213` → `$1.42`; not reported → `null`. */
export function usd(amount: number | null | undefined): string | null {
	if (amount == null || !Number.isFinite(amount)) return null;
	return `$${amount.toFixed(2)}`;
}

/**
 * The status bar / cost line for one session (G-93: never "— — ctx"). Both
 * figures missing → a single "—" carrying the tooltip.
 */
export function costLine(fig: { amt: string | null; ctx: string | null }): {
	text: string;
	unreported: boolean;
} {
	if (!fig.amt && !fig.ctx) return { text: '—', unreported: true };
	const parts = [fig.amt ?? '—'];
	if (fig.ctx) parts.push(`${fig.ctx} ctx`);
	return { text: parts.join(' · '), unreported: !fig.amt || !fig.ctx };
}

// ─── The iyke form (Principle 5, §7.3) ──────────────────────────────────────

const quote = (text: string) => `"${(text.trim() || '…').replace(/"/g, '\\"')}"`;

/** What ↵ would call for a seat target, with the draft in it. */
export function iykeSendToSeat(name: string, draft: string): string {
	return `terminal-send --seat ${name} ${quote(draft)}`;
}

export function iykeSendToTerminal(terminalId: string, draft: string): string {
	return `terminal-send --terminal ${terminalId} ${quote(draft)}`;
}

export function iykeChiRun(engineId: string, persistent: boolean, draft: string): string {
	return `chi run ${engineId}${persistent ? ' --persistent' : ''} --prompt ${quote(draft)}`;
}

export type CreateStart =
	| { kind: 'new' }
	| { kind: 'resume'; ref: string | null }
	| { kind: 'open'; ref: string | null };

/** The create form's line, with the form's exact flags (§7.3, R-14). */
export function iykeSeatCreate(name: string, engineId: string, start: CreateStart): string {
	const n = name.trim() || '<name>';
	if (start.kind === 'open') return `seat create ${n} --session ${start.ref ?? '<session id>'}`;
	if (start.kind === 'resume') {
		return `seat create ${n} --engine ${engineId} --resume ${start.ref ?? '<session id>'}`;
	}
	return `seat create ${n} --engine ${engineId}`;
}

/** The vacant panel's line. The bridge needs a prompt for both (§7.2 P-9). */
export function iykeVacant(name: string, resumable: boolean): string {
	return resumable ? `seat resume ${name} --prompt "…"` : `seat fill ${name} --prompt "…"`;
}
