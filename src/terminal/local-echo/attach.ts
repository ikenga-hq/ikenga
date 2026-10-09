/**
 * Wires a `PredictionEngine` to one xterm `Terminal`: keystrokes in, parsed
 * server output in, overlay out.
 *
 * Browser-only. `xterm-host.tsx` calls this only when the page is the SPA
 * running against `ikenga-server`; the desktop shell never constructs it, so
 * its terminals are byte-for-byte unchanged.
 *
 * One handle per xterm. Each viewer of a PTY (a pane, a popped-out window)
 * has its own xterm and so its own engine: keys are predicted only in the
 * viewer they were typed in, and nothing predicted ever enters the shared
 * byte stream, so a second viewer cannot see a double echo.
 */

import type { IDisposable, Terminal } from '@xterm/xterm';
import { type EngineStats, type LocalEchoMode, PredictionEngine } from './engine';
import { EchoOverlay } from './overlay';
import { useLocalEchoSettings } from './settings';
import { createXtermEchoTerm } from './xterm-adapter';

/** `localStorage[DEBUG_KEY] = '1'` exposes live handles on
 *  `window.__ikengaLocalEcho` (engine stats + the xterm), for field
 *  diagnosis and the latency harness in `scripts/local-echo/`. */
export const LOCAL_ECHO_DEBUG_KEY = 'ikenga.debug.localEcho';

export interface LocalEchoHandle {
	readonly engine: PredictionEngine;
	/** Call before writing `data` to the PTY. Returns the input's sequence number. */
	onInput(data: string): number;
	/** The PTY write for `seq` returned after `rttMs`. */
	ackInput(seq: number, rttMs: number): void;
	/**
	 * How to decide whether the app now in the alternate screen is a line
	 * editor we may predict in (see `allowAltScreenFor`). Null: never.
	 */
	setAltScreenProbe(probe: (() => Promise<boolean>) | null): void;
	/** Forget every prediction (the PTY behind this terminal changed). */
	reset(): void;
	stats(): EngineStats;
	dispose(): void;
}

export interface AttachOptions {
	/** Overrides the stored preference (tests). */
	mode?: LocalEchoMode;
	/** Clock override (tests). */
	now?: () => number;
}

interface DebugEntry {
	term: Terminal;
	stats: () => EngineStats;
}

function debugRegistry(): Set<DebugEntry> | null {
	try {
		if (localStorage.getItem(LOCAL_ECHO_DEBUG_KEY) !== '1') return null;
	} catch {
		return null;
	}
	const w = window as unknown as { __ikengaLocalEcho?: Set<DebugEntry> };
	w.__ikengaLocalEcho ??= new Set();
	return w.__ikengaLocalEcho;
}

export function attachLocalEcho(term: Terminal, opts: AttachOptions = {}): LocalEchoHandle {
	const disposables: IDisposable[] = [];
	let cursorHidden = false;
	let altAllowed = false;
	let probe: (() => Promise<boolean>) | null = null;
	let probeGen = 0;
	let timer: ReturnType<typeof setTimeout> | null = null;
	let disposed = false;

	const clock = opts.now ?? (() => performance.now());
	const echoTerm = createXtermEchoTerm(term, () => cursorHidden);
	const engine = new PredictionEngine(echoTerm, {
		mode: opts.mode ?? useLocalEchoSettings.getState().mode,
		allowAltScreen: () => altAllowed,
		now: clock,
	});
	const overlay = new EchoOverlay(term);

	const render = () => {
		if (disposed) return;
		overlay.render(engine.view());
		schedule();
	};

	const schedule = () => {
		if (timer) clearTimeout(timer);
		timer = null;
		const due = engine.nextDeadline();
		if (due === null) return;
		timer = setTimeout(
			() => {
				timer = null;
				engine.tick();
				render();
			},
			Math.max(0, due - clock()) + 1
		);
	};

	const runProbe = () => {
		const gen = ++probeGen;
		altAllowed = false;
		if (!probe || term.buffer.active.type !== 'alternate') return;
		probe().then(
			(ok) => {
				if (gen === probeGen && term.buffer.active.type === 'alternate') altAllowed = ok;
			},
			() => {}
		);
	};

	// DECTCEM: an app that hides the cursor draws its own, so the terminal
	// cursor says nothing about where the next character lands.
	disposables.push(
		term.parser.registerCsiHandler({ prefix: '?', final: 'l' }, (params) => {
			if (params.includes(25)) cursorHidden = true;
			return false;
		}),
		term.parser.registerCsiHandler({ prefix: '?', final: 'h' }, (params) => {
			if (params.includes(25)) cursorHidden = false;
			return false;
		}),
		// DECSTR and RIS both show the cursor again.
		term.parser.registerCsiHandler({ intermediates: '!', final: 'p' }, () => {
			cursorHidden = false;
			return false;
		}),
		term.parser.registerEscHandler({ final: 'c' }, () => {
			cursorHidden = false;
			return false;
		}),
		// Our own view of OSC 133; `false` lets `osc133.ts` handle it too.
		term.parser.registerOscHandler(133, (data) => {
			engine.onPromptMark(data.charAt(0));
			return false;
		}),
		term.onWriteParsed(() => {
			engine.onOutputParsed();
			render();
		}),
		term.onResize(() => {
			engine.onResize();
			render();
		}),
		term.buffer.onBufferChange(() => {
			engine.onBufferChange();
			runProbe();
			render();
		}),
		term.onScroll(() => overlay.refresh()),
		term.onRender(() => overlay.refresh())
	);

	const unsubscribeSettings = useLocalEchoSettings.subscribe((s) => {
		if (opts.mode) return;
		engine.setMode(s.mode);
		render();
	});

	const registry = debugRegistry();
	const debugEntry: DebugEntry = { term, stats: () => engine.stats() };
	registry?.add(debugEntry);

	return {
		engine,
		onInput(data) {
			const seq = engine.onInput(data);
			render();
			return seq;
		},
		ackInput(seq, rttMs) {
			engine.recordRtt(rttMs);
			engine.ackInput(seq);
			schedule();
		},
		setAltScreenProbe(next) {
			probe = next;
			runProbe();
		},
		reset() {
			engine.reset();
			render();
		},
		stats: () => engine.stats(),
		dispose() {
			if (disposed) return;
			disposed = true;
			if (timer) clearTimeout(timer);
			for (const d of disposables) d.dispose();
			unsubscribeSettings();
			registry?.delete(debugEntry);
			engine.dispose();
			overlay.dispose();
		},
	};
}

/**
 * The alternate-screen exception, decided from the foreground process.
 *
 * Full-screen apps are not predicted: their keys mostly do not echo where the
 * cursor is (vim normal mode, less, htop). Claude Code is the exception we
 * tested: from v2 it runs in the alternate screen, but its input box keeps
 * the real cursor visible at the caret and draws each typed character there,
 * the way a shell line editor does. The engine's own rules still apply on top
 * (tentative after every Enter, rollback on mismatch), so a key that does not
 * echo there — `!` or `?` on an empty prompt — is never shown.
 */
export function allowAltScreenFor(fg: { name: string; args: string[] } | null): boolean {
	if (!fg) return false;
	if (fg.name === 'claude') return true;
	// npm installs run as `node …/@anthropic-ai/claude-code/cli.js`.
	return fg.name === 'node' && fg.args.some((a) => /claude-code\/cli\.(m?js)$/.test(a));
}
