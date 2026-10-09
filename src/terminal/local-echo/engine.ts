/**
 * Predictive local echo — the prediction engine (mosh-style).
 *
 * In a browser tab talking to a remote `ikenga-server`, every keystroke
 * echoes only after a full round trip: `pty_write` → server PTY → `/ws/pty`
 * → xterm. This engine guesses what the echo will be, so the keystroke can be
 * shown at once, then checks each guess against what the server actually puts
 * on screen.
 *
 * ## Overlay, not buffer
 *
 * Predictions are never written into the xterm buffer. The engine only
 * describes them (`view()`); `overlay.ts` paints that description in a DOM
 * layer above the terminal. The buffer only ever holds bytes the server sent,
 * so a misprediction cannot leave residue and the final screen is, by
 * construction, the screen you would have had with prediction off. This is
 * also how mosh does it: its predictions are an overlay on the server's
 * framebuffer, not edits to it.
 *
 * ## What is predicted
 *
 * Only edits inside one row of a line editor: a printable single-width
 * character (inserted at the cursor, shifting the rest of the row right),
 * Backspace, and Left/Right arrows. Anything else — Enter, Tab, control keys,
 * other escape sequences, a paste, IME text, a wide character — is a
 * *barrier*: it is sent unpredicted, and prediction pauses until the server
 * has had time to act on it.
 *
 * ## When it is trusted (mosh's adaptive rule)
 *
 * Predictions belong to an *epoch*. A new epoch starts *tentative*: its
 * predictions are made and checked but not shown. The moment one of them is
 * confirmed by the server, the whole epoch is shown. Barriers, mispredictions,
 * resizes and buffer switches start a new tentative epoch. That is what keeps
 * a password prompt dark: the first key after Enter is tentative, the server
 * never echoes it, so nothing in that epoch is ever displayed.
 *
 * One shortcut on top of mosh: when the shell's own OSC 133 marks say we are
 * at its input line (`A` then `B`, seen after the last Enter), echo is known
 * to be on — the line editor draws it — so the epoch is trusted at once and
 * even the first key of a command shows instantly.
 *
 * Shown predictions that fail count against the engine; three within 30 s
 * suppress display for 20 s.
 *
 * ## Confirmation without an echo ack
 *
 * The server does not acknowledge keystrokes, so a prediction is *confirmed*
 * when the server's row shows the predicted text left of the cursor and the
 * cursor where we predicted it. It *fails* when the server has demonstrably
 * had its chance — the `pty_write` for it came back and the echo should have
 * followed — and the row still does not match.
 *
 * The engine is pure: no DOM, no timers. `attach.ts` wires it to xterm.
 */

export type LocalEchoMode = 'auto' | 'always' | 'off';

/** One cell of the server's screen, as the engine sees it. */
export interface CellView {
	/** The cell's text; `''` or `' '` for an empty cell. */
	ch: string;
	/**
	 * Faint text drawn by the app as a hint rather than input — a zsh
	 * autosuggestion, claude's placeholder. The app clears it on the next
	 * edit, so a prediction never shifts it and never insists on it.
	 */
	ghost: boolean;
}

/** A row the engine is following; `line` is -1 once the row is gone. */
export interface RowHandle {
	readonly line: number;
	dispose(): void;
}

/** What the engine needs from a terminal. `y` values are absolute buffer rows. */
export interface EchoTerm {
	readonly cols: number;
	isAltScreen(): boolean;
	/** DECTCEM off: the app draws its own cursor, so ours would be wrong. */
	isCursorHidden(): boolean;
	cursor(): { x: number; y: number };
	readRow(y: number): CellView[] | null;
	/** Follow row `y` through scrolling (an xterm marker). */
	trackRow(y: number): RowHandle;
}

export type KeyOp =
	| { kind: 'insert'; ch: string }
	| { kind: 'backspace' }
	| { kind: 'left' }
	| { kind: 'right' };

export type InputClass =
	| { type: 'op'; op: KeyOp }
	/** Terminal-generated reports and mouse motion: not keystrokes. */
	| { type: 'neutral' }
	/**
	 * Sent unpredicted. `hard` barriers may leave the line editor (Enter,
	 * ^C, ^D, ^Z, ^\, a paste containing a newline), so they also revoke the
	 * OSC 133 trust until the shell draws a fresh prompt.
	 */
	| { type: 'barrier'; hard: boolean };

const ESC = '\x1b';
const BEL = '\x07';
const ST = `${ESC}\\`;

/** Reports xterm sends on the app's behalf, matched after the leading ESC:
 *  CPR, DA, DECRPM, window reports, focus in/out. They arrive through the
 *  same `onData` as keystrokes but say nothing about the line being edited. */
const CSI_REPORT_RES: readonly RegExp[] = [
	/^\[\d+;\d+R$/,
	/^\[[?>=]?[\d;]*c$/,
	/^\[\??[\d;]*\$y$/,
	/^\[\d+(;\d+)*t$/,
	/^\[[IO]$/,
];

/** An SGR mouse report, after the leading ESC. */
const SGR_MOUSE_RE = /^\[<(\d+);\d+;\d+([Mm])$/;

function isTerminalReport(data: string): boolean {
	if (!data.startsWith(ESC)) return false;
	const rest = data.slice(1);
	// OSC and DCS replies (colour queries, DECRQSS).
	if (rest.startsWith(']')) return data.endsWith(BEL) || data.endsWith(ST);
	if (rest.startsWith('P')) return data.endsWith(ST);
	return CSI_REPORT_RES.some((re) => re.test(rest));
}

function hasNewline(s: string): boolean {
	return s.includes('\r') || s.includes('\n');
}

/**
 * Width-1 characters we are willing to predict: printable ASCII and the Latin
 * blocks a phone keyboard produces for accented letters. Anything wider or
 * stranger (CJK, emoji, combining marks) is left to the server.
 */
function isPredictableChar(ch: string): boolean {
	if (ch.length !== 1) return false;
	const c = ch.charCodeAt(0);
	return (c >= 0x20 && c <= 0x7e) || (c >= 0xa1 && c <= 0x24f);
}

export function classifyInput(data: string): InputClass {
	if (data.length === 1) {
		const c = data.charCodeAt(0);
		if (isPredictableChar(data)) return { type: 'op', op: { kind: 'insert', ch: data } };
		if (c === 0x7f || c === 0x08) return { type: 'op', op: { kind: 'backspace' } };
		// Enter, ^C, ^D, ^Z, ^\ can all leave the line editor.
		const hard = c === 0x0d || c === 0x0a || c === 0x03 || c === 0x04 || c === 0x1a || c === 0x1c;
		return { type: 'barrier', hard };
	}
	if (data === `${ESC}[D` || data === `${ESC}OD`) return { type: 'op', op: { kind: 'left' } };
	if (data === `${ESC}[C` || data === `${ESC}OC`) return { type: 'op', op: { kind: 'right' } };
	const mouse = data.startsWith(ESC) ? SGR_MOUSE_RE.exec(data.slice(1)) : null;
	if (mouse) {
		const button = Number(mouse[1]);
		// Motion (bit 5), wheel (bit 6) and release do not edit the input;
		// a press may move the caret (claude's input box does).
		if (button & 32 || button & 64 || mouse[2] === 'm') return { type: 'neutral' };
		return { type: 'barrier', hard: false };
	}
	if (isTerminalReport(data)) return { type: 'neutral' };
	// Paste (bracketed or not), IME commits, and every other escape sequence.
	return { type: 'barrier', hard: hasNewline(data) };
}

/**
 * The row text left of the cursor looks like a secret prompt. A second line of
 * defence behind the tentative-epoch rule, which already keeps an unechoed
 * password dark: this one also covers a prompt the engine is already trusting.
 */
const SECRET_PROMPT_RE =
	/(pass(word|phrase|code)?|passwd|\bpin\b|secret|token|verification code|otp)[^:]{0,40}[:?]\s*$/i;

interface Cell {
	ch: string;
	/** Typed by the user and not yet confirmed. */
	pred: boolean;
	ghost: boolean;
}

interface LineState {
	cells: Cell[];
	x: number;
}

interface PendingOp {
	op: KeyOp;
	seq: number;
	sentAt: number;
	ackedAt: number | null;
	epoch: number;
}

interface Model {
	row: RowHandle;
	alt: boolean;
	/** Backspace / Left may not cross this column. */
	floor: number;
	/** The server's row as of the last confirmation. */
	base: LineState;
	ops: PendingOp[];
	/** An unpredicted key was sent after `ops`; no more predictions on this
	 *  model, which is dropped once `ops` resolve. */
	barrier: boolean;
}

export interface OverlayCell {
	x: number;
	ch: string;
	/** A predicted keystroke (drawn dim + underlined) vs. surrounding text
	 *  the prediction shifted (drawn plain). */
	predicted: boolean;
	cursor: boolean;
}

export interface OverlayView {
	/** Absolute buffer row. */
	row: number;
	cells: OverlayCell[];
}

export interface EngineStats {
	mode: LocalEchoMode;
	/** Smoothed round trip, ms; null before the first sample. */
	srtt: number | null;
	rttvar: number | null;
	/** Whether predictions are currently being shown. */
	displaying: boolean;
	predicted: number;
	confirmed: number;
	/** Predictions that failed while shown. */
	failedVisible: number;
	/** Predictions that failed while tentative (never shown). */
	failedHidden: number;
	pending: number;
	epoch: number;
	confirmedEpoch: number;
	trustedPrompt: boolean;
}

export interface EngineOptions {
	mode?: LocalEchoMode;
	now?: () => number;
	/** May predictions run in the alternate screen right now? The host says
	 *  yes only for an app it has identified as a line editor (claude). */
	allowAltScreen?: () => boolean;
	/** Auto mode turns display on at or above this smoothed RTT… */
	autoOnMs?: number;
	/** …and back off below this one. */
	autoOffMs?: number;
}

const FAILURE_WINDOW_MS = 30_000;
const FAILURES_TO_SUPPRESS = 3;
const SUPPRESS_MS = 20_000;

function blank(ch: string): boolean {
	return ch === '' || ch === ' ';
}

function cloneState(s: LineState): LineState {
	return { cells: s.cells.map((c) => ({ ...c })), x: s.x };
}

function clearGhosts(s: LineState): void {
	for (const c of s.cells) {
		if (c.ghost) {
			c.ch = ' ';
			c.ghost = false;
		}
	}
}

function applyOp(s: LineState, op: KeyOp, cols: number): LineState {
	const next = cloneState(s);
	switch (op.kind) {
		case 'insert':
			clearGhosts(next);
			next.cells.splice(next.x, 0, { ch: op.ch, pred: true, ghost: false });
			next.cells.length = cols;
			next.x += 1;
			break;
		case 'backspace':
			clearGhosts(next);
			next.cells.splice(next.x - 1, 1);
			next.cells.push({ ch: ' ', pred: false, ghost: false });
			next.x -= 1;
			break;
		case 'left':
			next.x -= 1;
			break;
		case 'right':
			next.x += 1;
			break;
	}
	return next;
}

/** One past the last real (non-blank, non-ghost) character. */
function contentEnd(s: LineState): number {
	for (let i = s.cells.length - 1; i >= 0; i--) {
		const c = s.cells[i];
		if (!blank(c.ch) && !c.ghost) return i + 1;
	}
	return 0;
}

function textBefore(cells: readonly CellView[], x: number): string {
	let out = '';
	for (let i = 0; i < x && i < cells.length; i++) out += cells[i].ch || ' ';
	return out;
}

export class PredictionEngine {
	private mode: LocalEchoMode;
	private readonly now: () => number;
	private readonly allowAltScreen: () => boolean;
	private readonly autoOnMs: number;
	private readonly autoOffMs: number;

	private model: Model | null = null;
	private epoch = 1;
	private confirmedEpoch = 0;
	private seq = 0;
	/** Until this time keys sent unpredicted may still be in flight, so a
	 *  model built from the server's row may not include them. */
	private unsettledUntil = 0;

	private srtt: number | null = null;
	private rttvar: number | null = null;
	private autoOn = false;
	private failures: number[] = [];
	private suppressedUntil = 0;

	/** OSC 133: `A` seen since the last hard barrier. */
	private promptArmed = false;
	/** OSC 133: the shell's input line, once `B` follows an armed `A`. */
	private prompt: { row: RowHandle; col: number } | null = null;
	/** An unpredicted key went out since the last hard barrier. */
	private keysSinceHardBarrier = false;

	private counters = { predicted: 0, confirmed: 0, failedVisible: 0, failedHidden: 0 };

	constructor(
		private readonly term: EchoTerm,
		opts: EngineOptions = {}
	) {
		this.mode = opts.mode ?? 'auto';
		this.now = opts.now ?? (() => performance.now());
		this.allowAltScreen = opts.allowAltScreen ?? (() => false);
		this.autoOnMs = opts.autoOnMs ?? 80;
		this.autoOffMs = opts.autoOffMs ?? 50;
	}

	// ── configuration ────────────────────────────────────────────────────

	setMode(mode: LocalEchoMode): void {
		if (mode === this.mode) return;
		this.mode = mode;
		if (mode === 'off') this.reset();
	}

	getMode(): LocalEchoMode {
		return this.mode;
	}

	/** Are predictions being shown right now (vs. made and checked silently)? */
	isDisplaying(): boolean {
		if (this.mode === 'off') return false;
		if (this.now() < this.suppressedUntil) return false;
		return this.mode === 'always' || this.autoOn;
	}

	// ── RTT ──────────────────────────────────────────────────────────────

	/** One round-trip sample, ms (RFC 6298 smoothing). */
	recordRtt(ms: number): void {
		if (!Number.isFinite(ms) || ms < 0) return;
		if (this.srtt === null || this.rttvar === null) {
			this.srtt = ms;
			this.rttvar = ms / 2;
		} else {
			this.rttvar = 0.75 * this.rttvar + 0.25 * Math.abs(this.srtt - ms);
			this.srtt = 0.875 * this.srtt + 0.125 * ms;
		}
		if (!this.autoOn && this.srtt >= this.autoOnMs) this.autoOn = true;
		else if (this.autoOn && this.srtt < this.autoOffMs) this.autoOn = false;
	}

	/** How long after the last unpredicted key the server is assumed settled. */
	private settleMs(): number {
		if (this.srtt === null || this.rttvar === null) return 250;
		return this.srtt + 4 * this.rttvar + 50;
	}

	/** How long after its `pty_write` returns an echo may still be on its way. */
	private ackGraceMs(): number {
		const v = this.rttvar ?? 50;
		return Math.min(1500, Math.max(150, 2 * v + 120));
	}

	/**
	 * How long after it was sent an unacknowledged key's echo may still be on
	 * its way: one smoothed round trip plus four deviations (the TCP RTO
	 * formula) and a margin for the app itself. Keys typed over the PTY socket
	 * are never acknowledged, so this is the usual deadline in a browser.
	 */
	private expiryMs(): number {
		if (this.srtt === null || this.rttvar === null) return 1500;
		return Math.min(4000, Math.max(300, this.srtt + 4 * this.rttvar + 200));
	}

	// ── input ────────────────────────────────────────────────────────────

	/**
	 * A keystroke (or any `onData` chunk) is about to be written to the PTY.
	 * Returns its sequence number, to pass to `ackInput` when the write
	 * returns.
	 */
	onInput(data: string): number {
		const seq = ++this.seq;
		if (this.mode === 'off') return seq;
		const now = this.now();
		const cls = classifyInput(data);
		if (cls.type === 'neutral') return seq;
		if (cls.type === 'op' && this.tryPredict(cls.op, seq, now)) return seq;
		this.barrier(cls.type === 'barrier' && cls.hard, now);
		return seq;
	}

	/** The `pty_write` for `seq` returned: the server has the keystroke. */
	ackInput(seq: number): void {
		const now = this.now();
		const op = this.model?.ops.find((o) => o.seq === seq);
		if (op && op.ackedAt === null) op.ackedAt = now;
	}

	private tryPredict(op: KeyOp, seq: number, now: number): boolean {
		if (!this.model) this.model = this.startModel(now);
		const m = this.model;
		if (!m || m.barrier) return false;
		if (m.alt !== this.term.isAltScreen() || this.term.isCursorHidden()) return false;

		const states = this.states(m);
		const last = states[states.length - 1];
		const cols = this.term.cols;
		switch (op.kind) {
			case 'insert':
				// Never into the last column (wrap behaviour differs by app) and
				// never pushing real text off the row.
				if (last.x >= cols - 1) return false;
				if (!blank(last.cells[cols - 1].ch) && !last.cells[cols - 1].ghost) return false;
				break;
			case 'backspace':
			case 'left':
				if (last.x <= m.floor) return false;
				break;
			case 'right':
				if (last.x >= contentEnd(last)) return false;
				break;
		}
		m.ops.push({ op, seq, sentAt: now, ackedAt: null, epoch: this.epoch });
		this.counters.predicted += 1;
		return true;
	}

	/**
	 * Build a model from the server's current row, if prediction is sound.
	 *
	 * Before `unsettledUntil` the row may not yet show the effect of keys sent
	 * unpredicted (an Enter, a Tab), so a model built now may rest on a stale
	 * row. It is still built — continuous typing would otherwise never resume
	 * prediction — but its epoch is forced tentative: nothing it predicts is
	 * shown until the server confirms one of its guesses, which it can only
	 * do if the row was not stale after all.
	 */
	private startModel(now: number): Model | null {
		const settled = now >= this.unsettledUntil;
		const alt = this.term.isAltScreen();
		if (alt && !this.allowAltScreen()) return null;
		if (this.term.isCursorHidden()) return null;
		const cur = this.term.cursor();
		const cols = this.term.cols;
		if (cur.x >= cols) return null;
		const row = this.term.readRow(cur.y);
		if (!row) return null;
		if (SECRET_PROMPT_RE.test(`${textBefore(row, cur.x).trimEnd()} `)) return null;

		let floor = cur.x;
		if (!settled) this.becomeTentative();
		else if (this.prompt && this.isTrustedPrompt(cur)) {
			floor = this.prompt.col;
			// The line editor is drawing this row: echo is on.
			this.confirmedEpoch = Math.max(this.confirmedEpoch, this.epoch);
		}
		return {
			row: this.term.trackRow(cur.y),
			alt,
			floor,
			base: this.readState(row, cur.x),
			ops: [],
			barrier: false,
		};
	}

	private isTrustedPrompt(cur: { x: number; y: number }): boolean {
		const p = this.prompt;
		if (!p || this.term.isAltScreen()) return false;
		return p.row.line === cur.y && cur.x >= p.col;
	}

	private readState(row: readonly CellView[], x: number): LineState {
		const cols = this.term.cols;
		const cells: Cell[] = [];
		for (let i = 0; i < cols; i++) {
			const c = row[i];
			cells.push({ ch: c?.ch || ' ', pred: false, ghost: c?.ghost ?? false });
		}
		return { cells, x };
	}

	private barrier(hard: boolean, now: number): void {
		const m = this.model;
		if (m && m.ops.length > 0) m.barrier = true;
		else this.dropModel();
		this.unsettledUntil = Math.max(this.unsettledUntil, now + this.settleMs());
		this.becomeTentative();
		if (hard) {
			this.promptArmed = false;
			this.clearPrompt();
			this.keysSinceHardBarrier = false;
		} else {
			this.keysSinceHardBarrier = true;
		}
	}

	private becomeTentative(): void {
		if (this.epoch <= this.confirmedEpoch) this.epoch = this.confirmedEpoch + 1;
	}

	// ── OSC 133 ──────────────────────────────────────────────────────────

	/** An OSC 133 mark was parsed; the terminal cursor is where it was emitted. */
	onPromptMark(kind: string): void {
		switch (kind) {
			case 'A':
				this.promptArmed = true;
				this.clearPrompt();
				break;
			case 'B': {
				if (!this.promptArmed) break;
				this.promptArmed = false;
				const cur = this.term.cursor();
				this.clearPrompt();
				this.prompt = { row: this.term.trackRow(cur.y), col: cur.x };
				// A fresh prompt is an exact sync point — unless keys typed
				// ahead of it are still to be echoed onto it.
				if (!this.keysSinceHardBarrier) this.unsettledUntil = 0;
				break;
			}
			case 'C':
			case 'D':
				this.promptArmed = false;
				this.clearPrompt();
				break;
		}
	}

	private clearPrompt(): void {
		this.prompt?.row.dispose();
		this.prompt = null;
	}

	// ── server output ────────────────────────────────────────────────────

	/** Server bytes were parsed into the buffer: check every prediction. */
	onOutputParsed(): void {
		this.validate();
	}

	/** Timer tick: expire overdue predictions. */
	tick(): void {
		this.validate();
	}

	/** When the next prediction falls due, for the host's timer. */
	nextDeadline(): number | null {
		const op = this.model?.ops[0];
		return op ? this.deadline(op) : null;
	}

	private deadline(op: PendingOp): number {
		return op.ackedAt !== null ? op.ackedAt + this.ackGraceMs() : op.sentAt + this.expiryMs();
	}

	private validate(): void {
		const m = this.model;
		if (!m) return;
		const now = this.now();
		const line = m.row.line;
		if (line < 0 || m.alt !== this.term.isAltScreen()) {
			// Row trimmed out of scrollback, or the screen switched under us.
			this.fail(now, m.ops.length > 0);
			return;
		}
		const row = this.term.readRow(line);
		if (!row) {
			this.fail(now, m.ops.length > 0);
			return;
		}
		const cur = this.term.cursor();
		const onRow = cur.y === line;

		if (m.ops.length === 0) {
			if (!onRow) {
				// Output moved the cursor off the row: we no longer know it.
				this.dropModel();
				this.becomeTentative();
				return;
			}
			m.base = this.readState(row, cur.x);
			return;
		}

		const states = this.states(m);
		const n = m.ops.length;
		for (let k = n; k >= 1; k--) {
			if (!this.matchesExactly(row, cur, line, m.floor, states[k])) continue;
			this.confirm(m, k, states, row, cur, onRow, now);
			return;
		}
		// The key after the last prediction was a barrier (Enter, Tab) and the
		// server has already acted on it: the cursor has left the row, or moved
		// past where we put it (a completion extended the line). Then the
		// predicted text left of our cursor is all there is to check.
		const movedOn = !onRow || cur.x >= states[n].x;
		if (m.barrier && movedOn && this.matchesTextBefore(row, m.floor, states[n])) {
			this.confirm(m, n, states, row, cur, onRow, now);
			return;
		}

		if (
			m.ops.every((o) => o.epoch > this.confirmedEpoch) &&
			this.rowMoved(row, cur, line, m.base)
		) {
			// Nothing here was shown, and the server's row has changed in a way
			// none of our guesses explains: this model was built on a stale row.
			// Fail it now (silently) so the next key starts from the real one.
			this.fail(now, true);
			return;
		}
		if (now >= this.deadline(m.ops[0])) this.fail(now, true);
	}

	/** Cursor where predicted, and the row from `floor` through the cursor cell
	 *  as predicted (a ghost hint is allowed where we expect a blank). */
	private matchesExactly(
		row: readonly CellView[],
		cur: { x: number; y: number },
		line: number,
		floor: number,
		s: LineState
	): boolean {
		if (cur.y !== line || cur.x !== s.x) return false;
		if (!this.matchesTextBefore(row, floor, s)) return false;
		if (s.x >= this.term.cols) return true;
		const want = s.cells[s.x];
		const got = row[s.x] ?? { ch: ' ', ghost: false };
		return blank(want.ch) ? blank(got.ch) || got.ghost : want.ch === got.ch;
	}

	private matchesTextBefore(row: readonly CellView[], floor: number, s: LineState): boolean {
		for (let i = floor; i < s.x; i++) {
			const want = s.cells[i].ch;
			const got = row[i]?.ch ?? ' ';
			if (want !== got && !(blank(want) && blank(got))) return false;
		}
		return true;
	}

	private rowMoved(
		row: readonly CellView[],
		cur: { x: number; y: number },
		line: number,
		base: LineState
	): boolean {
		if (cur.y !== line || cur.x !== base.x) return true;
		for (let i = 0; i < base.cells.length; i++) {
			const was = base.cells[i].ch;
			const now = row[i]?.ch ?? ' ';
			if (was !== now && !(blank(was) && blank(now))) return true;
		}
		return false;
	}

	private confirm(
		m: Model,
		k: number,
		states: LineState[],
		row: readonly CellView[],
		cur: { x: number; y: number },
		onRow: boolean,
		now: number
	): void {
		const done = m.ops.slice(0, k);
		const last = done[k - 1];
		// A match that leaves the row as it was (Left then Right) proves nothing
		// about the server having seen the keys.
		const informative = !this.sameLine(states[k], states[0]);
		if (informative) {
			this.recordRtt(now - last.sentAt);
			this.confirmedEpoch = Math.max(this.confirmedEpoch, last.epoch);
		}
		this.counters.confirmed += k;
		m.ops = m.ops.slice(k);
		if (m.ops.length === 0 && m.barrier) {
			this.dropModel();
			return;
		}
		m.base = this.readState(row, onRow ? cur.x : states[k].x);
	}

	private sameLine(a: LineState, b: LineState): boolean {
		if (a.x !== b.x) return false;
		for (let i = 0; i < a.cells.length; i++) if (a.cells[i].ch !== b.cells[i].ch) return false;
		return true;
	}

	private fail(now: number, hadOps: boolean): void {
		const m = this.model;
		if (m && hadOps) {
			const shown = this.isDisplaying() && m.ops.some((o) => o.epoch <= this.confirmedEpoch);
			if (shown) {
				this.counters.failedVisible += m.ops.length;
				this.failures = this.failures.filter((t) => now - t < FAILURE_WINDOW_MS);
				this.failures.push(now);
				if (this.failures.length >= FAILURES_TO_SUPPRESS) {
					this.suppressedUntil = now + SUPPRESS_MS;
					this.failures = [];
				}
			} else {
				this.counters.failedHidden += m.ops.length;
			}
		}
		this.dropModel();
		this.becomeTentative();
	}

	private dropModel(): void {
		this.model?.row.dispose();
		this.model = null;
	}

	private states(m: Model): LineState[] {
		const out: LineState[] = [m.base];
		const cols = this.term.cols;
		for (const p of m.ops) out.push(applyOp(out[out.length - 1], p.op, cols));
		return out;
	}

	// ── lifecycle ────────────────────────────────────────────────────────

	/** Geometry changed: every position is stale and the app will redraw. */
	onResize(): void {
		this.dropModel();
		this.becomeTentative();
		this.unsettledUntil = Math.max(this.unsettledUntil, this.now() + this.settleMs());
	}

	/** Normal ↔ alternate screen. */
	onBufferChange(): void {
		this.dropModel();
		this.becomeTentative();
		if (this.term.isAltScreen()) this.clearPrompt();
	}

	/** Forget every prediction (PTY swapped, mode off). Keeps the RTT estimate. */
	reset(): void {
		this.dropModel();
		this.becomeTentative();
		this.clearPrompt();
		this.promptArmed = false;
	}

	dispose(): void {
		this.reset();
	}

	// ── output ───────────────────────────────────────────────────────────

	/**
	 * The cells to paint over the terminal, or null for nothing. Covers the
	 * span from the first to the last cell where the shown prediction differs
	 * from the server's row, plus both cursors — so the real cursor, which
	 * lags behind, is hidden under the prediction.
	 */
	view(): OverlayView | null {
		const m = this.model;
		if (!m || m.ops.length === 0 || !this.isDisplaying()) return null;
		const shown = m.ops.filter((o) => o.epoch <= this.confirmedEpoch);
		if (shown.length === 0) return null;
		const line = m.row.line;
		if (line < 0) return null;
		const row = this.term.readRow(line);
		if (!row) return null;
		const cols = this.term.cols;
		let s = m.base;
		for (const p of shown) s = applyOp(s, p.op, cols);

		const cur = this.term.cursor();
		let lo = Number.POSITIVE_INFINITY;
		let hi = Number.NEGATIVE_INFINITY;
		for (let i = 0; i < cols; i++) {
			const got = row[i]?.ch || ' ';
			const want = s.cells[i];
			if (got !== want.ch && !(blank(got) && blank(want.ch))) {
				lo = Math.min(lo, i);
				hi = Math.max(hi, i);
			}
		}
		lo = Math.min(lo, s.x);
		hi = Math.max(hi, s.x);
		if (cur.y === line) {
			lo = Math.min(lo, cur.x);
			hi = Math.max(hi, cur.x);
		}
		lo = Math.max(0, lo);
		hi = Math.min(cols - 1, hi);
		const cells: OverlayCell[] = [];
		for (let i = lo; i <= hi; i++) {
			const c = s.cells[i];
			cells.push({ x: i, ch: blank(c.ch) ? ' ' : c.ch, predicted: c.pred, cursor: i === s.x });
		}
		return { row: line, cells };
	}

	stats(): EngineStats {
		return {
			mode: this.mode,
			srtt: this.srtt,
			rttvar: this.rttvar,
			displaying: this.isDisplaying(),
			...this.counters,
			pending: this.model?.ops.length ?? 0,
			epoch: this.epoch,
			confirmedEpoch: this.confirmedEpoch,
			trustedPrompt: this.prompt !== null,
		};
	}
}
