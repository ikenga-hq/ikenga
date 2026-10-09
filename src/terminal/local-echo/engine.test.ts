/**
 * Prediction engine against a real xterm.js `Terminal` (parser + buffer, no
 * renderer). "Server" output is written into the terminal exactly as the PTY
 * stream would deliver it, so confirmation and rollback are judged against
 * real terminal semantics, not a mock of them.
 */

import { Terminal } from '@xterm/xterm';
import { afterEach, describe, expect, it } from 'vitest';
import { allowAltScreenFor, attachLocalEcho, type LocalEchoHandle } from './attach';
import { classifyInput, type LocalEchoMode } from './engine';

const PROMPT = '\x1b]133;A\x07$ \x1b]133;B\x07';
const RTT = 300;

class Rig {
	clock = 10_000;
	readonly term: Terminal;
	readonly le: LocalEchoHandle;
	/** Same server bytes, no prediction: the screen prediction must not change. */
	readonly reference: Terminal;
	private sent: { seq: number; at: number }[] = [];

	constructor(mode: LocalEchoMode = 'always', cols = 40) {
		this.term = new Terminal({ cols, rows: 8, allowProposedApi: true });
		this.reference = new Terminal({ cols, rows: 8, allowProposedApi: true });
		this.le = attachLocalEcho(this.term, { mode, now: () => this.clock });
	}

	async server(bytes: string): Promise<void> {
		await Promise.all([
			new Promise<void>((r) => this.term.write(bytes, r)),
			new Promise<void>((r) => this.reference.write(bytes, r)),
		]);
		this.le.engine.onOutputParsed();
	}

	type(keys: string | string[]): void {
		const list = typeof keys === 'string' ? [...keys] : keys;
		for (const k of list) {
			this.sent.push({ seq: this.le.onInput(k), at: this.clock });
			this.clock += 40;
		}
	}

	/** Every outstanding `pty_write` returns, one RTT after it was sent. */
	ackAll(): void {
		for (const s of this.sent) this.le.ackInput(s.seq, RTT);
		this.sent = [];
	}

	advance(ms: number): void {
		this.clock += ms;
		this.le.engine.tick();
	}

	/** What the overlay would paint: the row text with predictions applied. */
	shown(): string | null {
		const v = this.le.engine.view();
		if (!v) return null;
		const row = this.rowText(v.row).padEnd(this.term.cols).split('');
		for (const c of v.cells) row[c.x] = c.ch;
		return row.join('').trimEnd();
	}

	predictedCells(): string {
		return (this.le.engine.view()?.cells ?? [])
			.filter((c) => c.predicted)
			.map((c) => c.ch)
			.join('');
	}

	cursorCell(): number | null {
		return this.le.engine.view()?.cells.find((c) => c.cursor)?.x ?? null;
	}

	rowText(y?: number): string {
		const b = this.term.buffer.active;
		return b.getLine(y ?? b.baseY + b.cursorY)?.translateToString(true) ?? '';
	}

	/** Full buffer text of a terminal, for no-residue comparisons. */
	static dump(t: Terminal): string {
		const b = t.buffer.active;
		const out: string[] = [];
		for (let y = 0; y < b.length; y++) out.push(b.getLine(y)?.translateToString(true) ?? '');
		return `${out.join('\n')}\n@${b.cursorX},${b.cursorY}`;
	}

	assertSameAsReference(): void {
		expect(Rig.dump(this.term)).toBe(Rig.dump(this.reference));
	}

	dispose(): void {
		this.le.dispose();
		this.term.dispose();
		this.reference.dispose();
	}
}

let rigs: Rig[] = [];
function rig(mode?: LocalEchoMode, cols?: number): Rig {
	const r = new Rig(mode, cols);
	rigs.push(r);
	return r;
}

afterEach(() => {
	for (const r of rigs) r.dispose();
	rigs = [];
});

describe('classifyInput', () => {
	it('predicts printable keys, backspace and left/right only', () => {
		expect(classifyInput('a')).toEqual({ type: 'op', op: { kind: 'insert', ch: 'a' } });
		expect(classifyInput('é')).toEqual({ type: 'op', op: { kind: 'insert', ch: 'é' } });
		expect(classifyInput('\x7f')).toEqual({ type: 'op', op: { kind: 'backspace' } });
		expect(classifyInput('\x1b[D')).toEqual({ type: 'op', op: { kind: 'left' } });
		expect(classifyInput('\x1bOC')).toEqual({ type: 'op', op: { kind: 'right' } });
	});

	it('treats Enter and job-control keys as hard barriers', () => {
		for (const k of ['\r', '\n', '\x03', '\x04', '\x1a', '\x1c']) {
			expect(classifyInput(k)).toEqual({ type: 'barrier', hard: true });
		}
	});

	it('treats other control and escape keys as soft barriers', () => {
		for (const k of ['\t', '\x01', '\x1b', '\x1b[A', '\x1b[3~', '\x1b[H', '\x1bb']) {
			expect(classifyInput(k)).toEqual({ type: 'barrier', hard: false });
		}
	});

	it('never predicts a paste or IME commit, bracketed or not', () => {
		expect(classifyInput('\x1b[200~echo hi\x1b[201~')).toEqual({ type: 'barrier', hard: false });
		expect(classifyInput('\x1b[200~echo hi\r\x1b[201~')).toEqual({ type: 'barrier', hard: true });
		expect(classifyInput('ls -la')).toEqual({ type: 'barrier', hard: false });
		expect(classifyInput('你好')).toEqual({ type: 'barrier', hard: false });
		expect(classifyInput('😀')).toEqual({ type: 'barrier', hard: false });
	});

	it('ignores terminal reports and mouse motion, but not a click', () => {
		for (const k of ['\x1b[12;5R', '\x1b[?1;2c', '\x1b[I', '\x1b[O', '\x1b]11;rgb:0/0/0\x07']) {
			expect(classifyInput(k)).toEqual({ type: 'neutral' });
		}
		expect(classifyInput('\x1b[<35;10;5M')).toEqual({ type: 'neutral' });
		expect(classifyInput('\x1b[<64;10;5M')).toEqual({ type: 'neutral' });
		expect(classifyInput('\x1b[<0;10;5m')).toEqual({ type: 'neutral' });
		expect(classifyInput('\x1b[<0;10;5M')).toEqual({ type: 'barrier', hard: false });
	});
});

describe('insert → confirm', () => {
	it('shows keys at once at a shell prompt and confirms them on echo', async () => {
		const r = rig();
		await r.server(PROMPT);
		r.type('ls');
		expect(r.shown()).toBe('$ ls');
		expect(r.predictedCells()).toBe('ls');
		expect(r.cursorCell()).toBe(4);
		expect(r.rowText()).toBe('$ '); // the buffer itself is untouched

		await r.server('l');
		expect(r.shown()).toBe('$ ls');
		expect(r.predictedCells()).toBe('s');

		await r.server('s');
		expect(r.le.engine.view()).toBeNull();
		expect(r.le.stats()).toMatchObject({ predicted: 2, confirmed: 2, failedVisible: 0 });
		r.assertSameAsReference();
	});

	it('keeps the prediction across scrolling output above it', async () => {
		const r = rig();
		await r.server(`a\r\nb\r\nc\r\nd\r\ne\r\nf\r\ng\r\n${PROMPT}`);
		r.type('xy');
		await r.server('x');
		expect(r.shown()).toBe('$ xy');
		await r.server('y');
		expect(r.le.engine.view()).toBeNull();
		expect(r.le.stats().failedVisible).toBe(0);
	});

	it('confirms predictions an Enter overtook, without counting a failure', async () => {
		const r = rig();
		await r.server(PROMPT);
		r.type(['l', 's', '\r']);
		expect(r.shown()).toBe('$ ls');
		// The echo and the command's output arrive in one chunk; the cursor has
		// already left the row by the time the engine looks.
		await r.server(`ls\r\nfile1  file2\r\n${PROMPT}`);
		expect(r.le.engine.view()).toBeNull();
		expect(r.le.stats()).toMatchObject({ confirmed: 2, failedVisible: 0, failedHidden: 0 });
		r.assertSameAsReference();
	});
});

describe('rollback', () => {
	it('drops a prediction the server contradicts, leaving no residue', async () => {
		const r = rig();
		await r.server(PROMPT);
		r.type('abc');
		expect(r.shown()).toBe('$ abc');
		r.ackAll();
		// The app echoed something else (e.g. it upper-cases input).
		await r.server('ABC');
		r.advance(1000);
		expect(r.le.engine.view()).toBeNull();
		expect(r.le.stats().failedVisible).toBe(3);
		expect(r.rowText()).toBe('$ ABC');
		r.assertSameAsReference();
	});

	it('drops a prediction the server never echoes', async () => {
		const r = rig();
		await r.server(PROMPT);
		r.type('q');
		expect(r.shown()).toBe('$ q');
		r.ackAll();
		r.advance(2000);
		expect(r.le.engine.view()).toBeNull();
		r.assertSameAsReference();
	});

	it('without a write ack (PTY socket), expires one RTT + 4 deviations after sending', async () => {
		const r = rig();
		for (let i = 0; i < 20; i++) r.le.engine.recordRtt(300);
		await r.server(PROMPT);
		r.type('q'); // never acked: typed over the socket
		r.advance(400);
		expect(r.shown()).toBe('$ q'); // the echo may still be on its way
		r.advance(300);
		expect(r.le.engine.view()).toBeNull(); // 700 ms > 300 + 4·~0 + 200
		expect(r.le.stats().failedVisible).toBe(1);
		r.assertSameAsReference();
	});

	it('becomes tentative after a shown misprediction (mosh rule)', async () => {
		const r = rig();
		await r.server('> '); // no OSC 133: untrusted line
		r.type('a');
		expect(r.shown()).toBeNull(); // tentative: first key waits for its echo
		await r.server('a');
		r.type('b');
		expect(r.shown()).toBe('> ab'); // epoch confirmed: shown at once
		r.ackAll();
		await r.server('X');
		r.advance(2000);
		expect(r.le.stats().failedVisible).toBe(1);
		r.type('c');
		expect(r.shown()).toBeNull(); // back to tentative
		await r.server('c');
		r.type('d');
		expect(r.shown()).toBe('> aXcd');
	});

	it('suppresses display after repeated shown failures', async () => {
		const r = rig();
		await r.server(PROMPT);
		for (let i = 0; i < 3; i++) {
			r.type('z');
			// Re-earn trust each round with a confirmed key.
			if (!r.le.engine.view()) {
				await r.server('z');
				r.type('z');
			}
			r.ackAll();
			await r.server('#');
			r.advance(2000);
		}
		expect(r.le.stats().failedVisible).toBeGreaterThanOrEqual(3);
		expect(r.le.stats().displaying).toBe(false);
		r.advance(21_000);
		expect(r.le.stats().displaying).toBe(true);
	});
});

describe('never predicts when likely wrong', () => {
	it('keeps an echo-off password prompt dark', async () => {
		const r = rig();
		await r.server(PROMPT);
		r.type([...'sudo ls', '\r']);
		await r.server('sudo ls\r\n\x1b]133;C\x07[sudo] password for me: ');
		r.type('hunter2');
		expect(r.shown()).toBeNull();
		r.ackAll();
		r.advance(3000);
		expect(r.le.engine.view()).toBeNull();
		expect(r.le.stats().failedVisible).toBe(0);
		r.assertSameAsReference();
	});

	it('keeps an unrecognised echo-off prompt dark too (tentative epoch)', async () => {
		const r = rig();
		await r.server(PROMPT);
		r.type([...'./unlock', '\r']);
		await r.server('./unlock\r\n\x1b]133;C\x07Enter key: ');
		r.type('s3cr3t');
		expect(r.shown()).toBeNull();
		r.ackAll();
		r.advance(3000);
		expect(r.le.stats()).toMatchObject({ failedVisible: 0 });
		expect(r.le.stats().failedHidden).toBeGreaterThan(0);
		r.type('x');
		expect(r.shown()).toBeNull();
	});

	it('does not trust a prompt until the shell redraws it after Enter', async () => {
		const r = rig();
		await r.server(PROMPT);
		r.type([...'sudo -k', '\r', ...'pw']); // typed ahead, before the server answers
		expect(r.predictedCells()).toBe('sudo -k'.replace(/ /g, ' '));
		await r.server('sudo -k\r\n\x1b]133;C\x07Password: ');
		expect(r.shown()).toBeNull();
	});

	it('does not predict in the alternate screen', async () => {
		const r = rig();
		await r.server('\x1b[?1049h\x1b[H~\r\n~\x1b[H');
		r.type('jjk');
		expect(r.shown()).toBeNull();
		expect(r.le.stats().predicted).toBe(0);
	});

	it('predicts in the alternate screen only for an allowed app, and tentatively', async () => {
		const r = rig();
		r.le.setAltScreenProbe(async () => true);
		await r.server('\x1b[?1049h\x1b[H─────\r\n❯ \r\n─────\x1b[2;3H');
		await Promise.resolve();
		r.type('h');
		expect(r.shown()).toBeNull(); // first key after entering: tentative
		await r.server('h');
		r.type('i');
		expect(r.shown()).toBe('❯ hi');
	});

	it('does not predict while the app hides the cursor', async () => {
		const r = rig();
		await r.server(`${PROMPT}\x1b[?25l`);
		r.type('abc');
		expect(r.le.stats().predicted).toBe(0);
		await r.server('\x1b[?25h');
		r.advance(1000);
		r.type('d');
		expect(r.shown()).toBe('$ d');
	});

	it('never predicts a bracketed paste, and resumes after it', async () => {
		const r = rig();
		await r.server(`${PROMPT}\x1b[?2004h`);
		r.type(['\x1b[200~echo pasted\x1b[201~']);
		expect(r.shown()).toBeNull();
		expect(r.le.stats().predicted).toBe(0);
		r.type('x');
		expect(r.shown()).toBeNull(); // tentative: the paste may not have landed yet
		// The paste lands: the model 'x' was guessed on is stale, and is
		// dropped silently rather than left to expire.
		await r.server('echo pasted');
		expect(r.le.stats()).toMatchObject({ failedVisible: 0, failedHidden: 1 });
		await r.server('x');
		r.type('y');
		expect(r.shown()).toBeNull(); // one confirmation re-earns trust…
		await r.server('y');
		r.type('z');
		expect(r.shown()).toBe('$ echo pastedxyz'); // …then it is instant again
	});

	it('does not predict control or escape keys', async () => {
		const r = rig();
		await r.server(PROMPT);
		r.type(['\t', '\x1b[A', '\x01', '\x1b']);
		expect(r.le.stats().predicted).toBe(0);
		expect(r.shown()).toBeNull();
	});

	it('never predicts into the last column', async () => {
		const r = rig('always', 10);
		await r.server(PROMPT);
		r.type('abcdefgh');
		// cols 2..8 predicted; col 9 (last) is left to the server.
		expect(r.predictedCells()).toBe('abcdefg');
	});
});

describe('editing inside the predicted region', () => {
	it('predicts backspace, and never past the prompt', async () => {
		const r = rig();
		await r.server(PROMPT);
		r.type(['a', 'b', 'c', '\x7f']);
		expect(r.shown()).toBe('$ ab');
		expect(r.cursorCell()).toBe(4);
		r.type(['\x7f', '\x7f', '\x7f']); // the third would delete into the prompt
		expect(r.shown()).toBe('$');
		expect(r.le.stats().predicted).toBe(6);
		// readline's echo for the same keys
		await r.server('abc\b \b\b \b\b \b');
		expect(r.le.engine.view()).toBeNull();
		expect(r.le.stats().failedVisible).toBe(0);
	});

	it('predicts left/right and inserts mid-line, shifting the rest', async () => {
		const r = rig();
		await r.server(PROMPT);
		r.type(['a', 'b', 'c', '\x1b[D', '\x1b[D', 'X']);
		expect(r.shown()).toBe('$ aXbc');
		expect(r.cursorCell()).toBe(4);
		r.type(['\x1b[C', '\x1b[C', '\x1b[C']); // the third would pass the end of input
		expect(r.cursorCell()).toBe(6);
		expect(r.le.stats().predicted).toBe(8);
		// readline: echo abc, two cursor-lefts, then insert redraws the tail
		await r.server('abc\b\bXbc\b\b');
		expect(r.shown()).toBe('$ aXbc');
		await r.server('\x1b[C\x1b[C');
		expect(r.le.engine.view()).toBeNull();
		expect(r.le.stats().failedVisible).toBe(0);
		r.assertSameAsReference();
	});

	it('confirms past a zsh-style ghost suggestion', async () => {
		const r = rig();
		await r.server(PROMPT);
		r.type('gi');
		await r.server('g\x1b[90mit status\x1b[39m\x1b[9D');
		expect(r.shown()).toBe('$ gi'); // the ghost is hidden under the prediction
		await r.server('i\x1b[90mt status\x1b[39m\x1b[8D');
		expect(r.le.engine.view()).toBeNull();
		expect(r.le.stats().failedVisible).toBe(0);
	});
});

describe('regressions from independent review', () => {
	it('does not trust a fresh model while failed keys may still echo', async () => {
		const r = rig();
		for (let i = 0; i < 20; i++) r.le.engine.recordRtt(100);
		await r.server(PROMPT);
		r.type('ab');
		await r.server('ab');
		r.type('cd'); // echo delayed past expiry
		r.advance(1000);
		expect(r.le.engine.view()).toBeNull();
		r.type('e');
		expect(r.shown()).toBeNull(); // not '$ abe' over a stale row
		await r.server('cde');
		r.type('f');
		await r.server('f');
		r.type('g');
		expect(r.shown()).toBe('$ abcdefg');
		r.assertSameAsReference();
	});

	it("treats fish's truecolour-grey autosuggestion as a hint", async () => {
		const r = rig();
		await r.server(`${PROMPT}git \x1b[38;2;85;85;85mstatus\x1b[39m\x1b[6D`);
		r.type('s');
		expect(r.shown()).toBe('$ git s');
		await r.server('s\x1b[38;2;85;85;85mtatus\x1b[39m\x1b[5D');
		expect(r.le.engine.view()).toBeNull();
		expect(r.le.stats()).toMatchObject({ confirmed: 1, failedVisible: 0 });
	});

	it('leaves a right-aligned prompt where it is and keeps predicting', async () => {
		const r = rig();
		await r.server(`${PROMPT}\x1b7\x1b[1;30H[12:00:00]\x1b8`); // ends at cols-2
		r.type('abc');
		expect(r.le.stats().pending).toBe(3);
		expect(r.shown()).toBe('$ abc                        [12:00:00]');
		await r.server('abc');
		expect(r.le.engine.view()).toBeNull();
		expect(r.le.stats().failedVisible).toBe(0);
	});
});

describe('geometry and lifecycle', () => {
	it('drops predictions on resize', async () => {
		const r = rig();
		await r.server(PROMPT);
		r.type('ab');
		expect(r.shown()).toBe('$ ab');
		r.term.resize(30, 8);
		expect(r.le.engine.view()).toBeNull();
		r.assertSameAsReference();
	});

	it('drops predictions when the screen switches buffers', async () => {
		const r = rig();
		await r.server(PROMPT);
		r.type('ab');
		await r.server('\x1b[?1049h');
		expect(r.le.engine.view()).toBeNull();
	});

	it('mode off: no predictions at all', async () => {
		const r = rig('off');
		await r.server(PROMPT);
		r.type('ab');
		expect(r.le.stats().predicted).toBe(0);
		expect(r.shown()).toBeNull();
	});
});

describe('auto mode', () => {
	it('stays dark below the threshold and lights up above it (with hysteresis)', async () => {
		const r = rig('auto');
		await r.server(PROMPT);
		for (let i = 0; i < 5; i++) r.le.engine.recordRtt(20);
		r.type('a');
		expect(r.shown()).toBeNull();
		expect(r.le.stats().predicted).toBe(1); // still predicted and checked, silently
		await r.server('a');
		for (let i = 0; i < 20; i++) r.le.engine.recordRtt(300);
		expect(r.le.stats().displaying).toBe(true);
		r.type('b');
		expect(r.shown()).toBe('$ ab');
		for (let i = 0; i < 4; i++) r.le.engine.recordRtt(60); // between 50 and 80
		expect(r.le.stats().displaying).toBe(true);
		for (let i = 0; i < 30; i++) r.le.engine.recordRtt(10);
		expect(r.le.stats().displaying).toBe(false);
	});
});

describe('multiple viewers of one PTY', () => {
	it('predicts only in the viewer that typed, with no double echo anywhere', async () => {
		const a = rig();
		const b = rig();
		const both = async (s: string) => {
			await a.server(s);
			await b.server(s);
		};
		await both(PROMPT);
		a.type('hey');
		expect(a.shown()).toBe('$ hey');
		expect(b.le.engine.view()).toBeNull();
		await both('he');
		await both('y');
		expect(a.le.engine.view()).toBeNull();
		expect(b.le.engine.view()).toBeNull();
		expect(Rig.dump(a.term)).toBe(Rig.dump(b.term));
		expect(a.rowText()).toBe('$ hey');
	});

	it("rolls back cleanly when another viewer's keys land first", async () => {
		const a = rig();
		const b = rig();
		await a.server(PROMPT);
		await b.server(PROMPT);
		a.type('ab');
		a.ackAll();
		// B typed 'zz' and the server processed it before A's keys.
		for (const t of [a, b]) await t.server('zzab');
		a.advance(1000);
		expect(a.le.engine.view()).toBeNull();
		expect(Rig.dump(a.term)).toBe(Rig.dump(b.term));
		a.assertSameAsReference();
	});
});

describe('allowAltScreenFor', () => {
	it('allows claude (native or npm) and nothing else', () => {
		expect(allowAltScreenFor({ name: 'claude', args: ['claude'] })).toBe(true);
		expect(
			allowAltScreenFor({
				name: 'node',
				args: ['node', '/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js'],
			})
		).toBe(true);
		expect(allowAltScreenFor({ name: 'vim', args: ['vim'] })).toBe(false);
		expect(allowAltScreenFor({ name: 'node', args: ['node', 'server.js'] })).toBe(false);
		expect(allowAltScreenFor(null)).toBe(false);
	});
});
