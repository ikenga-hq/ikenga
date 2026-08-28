import { FitAddon } from '@xterm/addon-fit';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { WebglAddon } from '@xterm/addon-webgl';
import { type ITheme, Terminal } from '@xterm/xterm';
import { useEffect, useRef, useState } from 'react';
import { OS_FILE_DROP_EVENT, type OsFileDropDetail } from '@/lib/dnd/os-file-drop';
import { usePaneStore } from '@/lib/panes/pane-store';
import { fileUrlToPath, resolvePath } from '@/lib/paths/file-paths';
import { createOscObserver, fireOscNotification } from '@/lib/terminal/osc-notify';
import {
	evaluateTerminalKey,
	getDefaultKeybindings,
	type TerminalKeybindings,
} from './keybindings';
import { registerPathLinks } from './path-links';
import { Pty, type PtySpawnOpts } from './pty-bridge';
import { readCaptureWithOffset } from './pty-output-buffer';
import { useTerminalStore } from './session-store';

export interface TerminalSpec {
	cwd: string;
	cmd: string[];
	env?: Record<string, string>;
}

interface Props {
	/**
	 * Spawn-mode: provide a spec and the host will spawn its own PTY (lifecycle
	 * tied to the component). Used for one-off terminals.
	 */
	spec?: TerminalSpec | null;
	/**
	 * Attach-mode: provide an existing PTY (managed externally, e.g. by the
	 * session store) and the host will only render. The PTY survives unmount.
	 */
	pty?: Pty | null;
	onStatus?: (s: string) => void;
	onExit?: (code: number | null) => void;
	/** Called once a PTY has been created in spawn-mode. */
	onPtyId?: (id: string) => void;
	/**
	 * Force the DOM renderer instead of WebGL. Detached windows
	 * (plans/multi-window WP-08) set this: WebGL "loads" in a secondary
	 * WebKitGTK webview but renders no glyphs (only the cursor) without ever
	 * firing onContextLoss, so the auto-fallback never triggers. DOM renderer works.
	 */
	disableWebgl?: boolean;
	/**
	 * Stable identity for the underlying terminal SESSION (not the PTY —
	 * `pty.id` changes across a restart, `sessionId` doesn't). When set
	 * (together with `pty`), the host reuses a module-scope cached
	 * `Terminal` + container `<div>` across remounts instead of building a
	 * fresh xterm wired only to future bytes — see the module-scope cache
	 * below. Omit for spawn-mode / detached-window usage, where every mount
	 * legitimately owns its own terminal.
	 */
	sessionId?: string;
	/**
	 * Whether the pane hosting this terminal currently has focus. Only
	 * consulted on a cache-hit remount (re-parenting a previously-cached
	 * terminal) to decide whether to steal DOM focus; a fresh terminal
	 * always focuses on creation, matching prior behavior.
	 */
	focused?: boolean;
	/**
	 * T-2 (plans/multi-window "corruption 2 (reflow)"): opt-in, one-shot
	 * repaint nudge for a detached-window attach. Set ONLY from
	 * `detached/surfaces/terminal-surface.tsx`. A popped-out window attaches
	 * to a PTY that may already be sized to match (the replayed scrollback
	 * landed fine), which means the PTY-side resize this window's own fit()
	 * issues can be a same-size no-op — Linux's tty layer drops the SIGWINCH
	 * for an unchanged winsize, so a full-screen TUI (vim, htop, claude
	 * itself) never gets told to repaint at the new window's geometry and
	 * stays visually corrupted even though the byte stream is correct. See
	 * `scheduleAttachNudge` below for the two-step wobble that forces it.
	 */
	nudgeOnAttach?: boolean;
	/** Configurable scrollback lines (defaults to 10,000). */
	scrollback?: number;
	/** Custom terminal keybinding overrides (T-11). */
	keybindings?: Partial<TerminalKeybindings>;
}

const DARK_THEME: ITheme = {
	background: '#0a0a0a',
	foreground: '#e6e6e6',
	cursor: '#e6e6e6',
	cursorAccent: '#000000',
	selectionBackground: '#3a3d41',
	black: '#000000',
	red: '#cd3131',
	green: '#0dbc79',
	yellow: '#e5e510',
	blue: '#2472c8',
	magenta: '#bc3fbc',
	cyan: '#11a8cd',
	white: '#e5e5e5',
	brightBlack: '#666666',
	brightRed: '#f14c4c',
	brightGreen: '#23d18b',
	brightYellow: '#f5f543',
	brightBlue: '#3b8eea',
	brightMagenta: '#d670d6',
	brightCyan: '#29b8db',
	brightWhite: '#e5e5e5',
};

const LIGHT_THEME: ITheme = {
	background: '#ffffff',
	foreground: '#1f2328',
	cursor: '#1f2328',
	cursorAccent: '#ffffff',
	selectionBackground: '#cce0ff',
	black: '#24292f',
	red: '#cf222e',
	green: '#116329',
	yellow: '#4d2d00',
	blue: '#0969da',
	magenta: '#8250df',
	cyan: '#1b7c83',
	white: '#6e7781',
	brightBlack: '#57606a',
	brightRed: '#a40e26',
	brightGreen: '#1a7f37',
	brightYellow: '#633c01',
	brightBlue: '#218bff',
	brightMagenta: '#a475f9',
	brightCyan: '#3192aa',
	brightWhite: '#8c959f',
};

function isDarkMode(): boolean {
	if (typeof document === 'undefined') return true;
	return document.documentElement.classList.contains('dark');
}

function isMac(): boolean {
	if (typeof navigator === 'undefined') return false;
	// navigator.platform is deprecated but still works; fall back to userAgent.
	const p = navigator.platform || navigator.userAgent || '';
	return /Mac|iPhone|iPad/.test(p);
}

/**
 * Pick up theme overrides from CSS custom properties on :root if they exist.
 * Falls back to our hard-coded palette when a token is missing/empty.
 */
function readThemeFromCssVars(dark: boolean): ITheme {
	const base = dark ? DARK_THEME : LIGHT_THEME;
	if (typeof document === 'undefined') return base;
	const style = getComputedStyle(document.documentElement);
	const bg = style.getPropertyValue('--color-background').trim();
	const fg = style.getPropertyValue('--color-foreground').trim();
	return {
		...base,
		...(bg ? { background: bg } : {}),
		...(fg ? { foreground: fg, cursor: fg } : {}),
	};
}

// ---------------------------------------------------------------------------
// Module-scope xterm cache — mirrors `route-view.tsx`'s `routerCache` idiom.
//
// Keyed by terminal SESSION id (the terminal-store tab id, stable across a
// PTY restart), not by `pty.id`. Holds the live `Terminal` + its container
// `<div>` + every PTY-facing subscription so a pane-tree remount (tab
// switch/reorder/split/close — see plans/studio/17-deep-review §1) can
// re-parent the existing DOM node and resume writing instead of building a
// fresh `Terminal` wired only to future bytes (which made a live PTY look
// "restarted": scrollback + TUI screen state were discarded every remount).
//
// Per-mount concerns (key handler rebinding to this render's React state,
// theme/resize observers, the fit-retry loop) are NOT part of the cache —
// those are cheap to recreate and some (the key handler) MUST be rebound
// every mount since they close over this render's `setState`/refs.
// ---------------------------------------------------------------------------

interface SearchOptions {
	regex?: boolean;
	wholeWord?: boolean;
	caseSensitive?: boolean;
	incremental?: boolean;
	decorations?: {
		matchBackground?: string;
		matchBorder?: string;
		matchOverviewRuler: string;
		activeMatchBackground?: string;
		activeMatchBorder?: string;
		activeMatchColorOverviewRuler: string;
	};
}

interface SearchResultChangeEvent {
	resultIndex: number;
	resultCount: number;
}

interface SearchAddonLike {
	findNext: (s: string, options?: SearchOptions) => boolean;
	findPrevious: (s: string, options?: SearchOptions) => boolean;
	onDidChangeResults?: (listener: (results: SearchResultChangeEvent) => void) => {
		dispose: () => void;
	};
	clearDecorations?: () => void;
	dispose: () => void;
}

interface XTermCacheEntry {
	term: Terminal;
	container: HTMLDivElement;
	fit: FitAddon;
	webglAddon: WebglAddon | null;
	webglUsed: boolean;
	searchAddon: SearchAddonLike | null;
	pathLinksDispose: () => void;
	oscObserver: ReturnType<typeof createOscObserver>;
	/** `pty.id` currently wired to `term`. Differs from a fresh `pty.id` after
	 *  a restart (same session, new process) — triggers a listener rewire. */
	wiredPtyId: string;
	detachData: () => void;
	detachExit: () => void;
	onDataDispose: { dispose: () => void };
	onResizeDispose: { dispose: () => void };
}

const xtermCache = new Map<string, XTermCacheEntry>();

function disposeCacheEntry(entry: XTermCacheEntry): void {
	try {
		entry.detachData();
	} catch {
		/* ignore */
	}
	try {
		entry.detachExit();
	} catch {
		/* ignore */
	}
	try {
		entry.onDataDispose.dispose();
	} catch {
		/* ignore */
	}
	try {
		entry.onResizeDispose.dispose();
	} catch {
		/* ignore */
	}
	try {
		entry.searchAddon?.dispose();
	} catch {
		/* ignore */
	}
	try {
		entry.pathLinksDispose();
	} catch {
		/* ignore */
	}
	try {
		entry.webglAddon?.dispose();
	} catch {
		/* ignore */
	}
	try {
		entry.term.dispose();
	} catch {
		/* xterm sometimes throws when renderer is mid-frame; safe to drop */
	}
}

function evictXtermCache(sessionId: string): void {
	const entry = xtermCache.get(sessionId);
	if (!entry) return;
	xtermCache.delete(sessionId);
	disposeCacheEntry(entry);
}

// Evict cache entries whose session the terminal store no longer tracks
// (tab removed — the session actually closed, not just a pane remount). One
// global subscription; terminal-tab churn is infrequent.
useTerminalStore.subscribe((state) => {
	const liveIds = new Set(state.tabs.map((t) => t.id));
	for (const id of Array.from(xtermCache.keys())) {
		if (!liveIds.has(id)) evictXtermCache(id);
	}
});

// HMR: a code change to this module would otherwise leave cached `Terminal`
// instances + their PTY listeners wired against closures from the previous
// module instance (stale `status`/`exit` refs, a `term.write` target the
// next module load can't reach). Dispose the whole cache right before the
// module is replaced so the next mount rebuilds clean — mirrors the
// `import.meta.hot.accept` guard in `route-view.tsx`, using `dispose` here
// since it's this module (not an imported one) that's being swapped.
if (import.meta.hot) {
	import.meta.hot.dispose(() => {
		for (const entry of xtermCache.values()) disposeCacheEntry(entry);
		xtermCache.clear();
	});
}

/** Wire a `Terminal` to a `Pty`'s data/exit/resize streams. Used both when a
 *  cache entry is created and when an existing entry's underlying PTY
 *  identity changes (restart) and needs rewiring. */
function wirePtyToTerm(
	term: Terminal,
	pty: Pty,
	oscObserver: ReturnType<typeof createOscObserver>,
	status: (s: string) => void,
	exit: (code: number | null) => void,
	webglUsed: boolean
): {
	detachData: () => void;
	detachExit: () => void;
	onDataDispose: { dispose: () => void };
	onResizeDispose: { dispose: () => void };
} {
	const dataHandler = (bytes: Uint8Array) => {
		oscObserver.feed(bytes);
		term.write(bytes);
	};
	const exitHandler = (code: number | null) => {
		// VS Code pattern: keep the canvas mounted, write an inline notice as
		// the last line so anything the process *did* emit stays visible
		// above.
		try {
			const codeStr = code === null ? '?' : String(code);
			const hint =
				code !== null && code !== 0
					? '  (check command args or whether the --resume session id is still valid)'
					: '';
			term.writeln('');
			term.writeln(`\x1b[2m[process exited with code ${codeStr}]${hint}\x1b[0m`);
		} catch {
			/* terminal may be mid-dispose */
		}
		status(`pty exited (code=${code ?? '?'})`);
		exit(code);
	};
	const detachData = pty.onData(dataHandler);
	const detachExit = pty.onExit(exitHandler);
	const onDataDispose = term.onData((data) => {
		pty.write(data).catch(console.error);
	});
	const onResizeDispose = term.onResize(({ rows, cols }) => {
		pty.resize(rows, cols).catch(console.error);
	});
	// Sync initial size to PTY (in case we attached/rewired at a different
	// terminal geometry than the PTY currently has).
	try {
		pty.resize(term.rows, term.cols).catch(() => {});
	} catch {
		/* ignore */
	}
	status(`pty ${pty.id.slice(0, 8)} ${pty.label} (${webglUsed ? 'webgl' : 'dom'})`);
	return { detachData, detachExit, onDataDispose, onResizeDispose };
}

/** Quote a filesystem path for a POSIX shell: wrap in single quotes and
 *  escape any embedded single quote as `'\''`. Dropping a path with a space
 *  into a shell is useless unquoted. */
function shellQuote(path: string): string {
	return `'${path.replace(/'/g, `'\\''`)}'`;
}

export function XTermHost({
	spec,
	pty,
	onStatus,
	onExit,
	onPtyId,
	disableWebgl,
	sessionId,
	focused,
	nudgeOnAttach,
	scrollback,
	keybindings,
}: Props) {
	const containerRef = useRef<HTMLDivElement | null>(null);
	const wrapperRef = useRef<HTMLDivElement | null>(null);
	const [searchOpen, setSearchOpen] = useState(false);
	const [searchTerm, setSearchTerm] = useState('');
	const [caseSensitive, setCaseSensitive] = useState(false);
	const [wholeWord, setWholeWord] = useState(false);
	const [useRegex, setUseRegex] = useState(false);
	const [searchResult, setSearchResult] = useState<SearchResultChangeEvent | null>(null);
	const searchInputRef = useRef<HTMLInputElement | null>(null);

	// Stash callbacks in refs so the spawn effect doesn't re-fire on each render.
	const onStatusRef = useRef(onStatus);
	const onExitRef = useRef(onExit);
	const onPtyIdRef = useRef(onPtyId);
	const focusedRef = useRef(focused);
	const keybindingsRef = useRef(keybindings);
	onStatusRef.current = onStatus;
	onExitRef.current = onExit;
	onPtyIdRef.current = onPtyId;
	focusedRef.current = focused;
	keybindingsRef.current = keybindings;
	const status = (s: string) => onStatusRef.current?.(s);
	const exit = (code: number | null) => onExitRef.current?.(code);

	// Hold the search addon ref so the inline search input can drive it.
	const searchAddonRef = useRef<SearchAddonLike | null>(null);
	const termRef = useRef<Terminal | null>(null);
	// Whichever PTY is currently live — the `pty` prop (attach mode) or the
	// internally-spawned one (spec mode). The drop handler writes the dropped
	// file's path here.
	const livePtyRef = useRef<Pty | null>(null);

	useEffect(() => {
		// We must have either a spec (spawn) or a pty (attach) to render.
		if (!spec && !pty) return;
		if (!containerRef.current) return;

		const mountEl = containerRef.current;
		let cancelled = false;
		let disposed = false;
		const pendingRafs = new Set<number>();
		const pendingTimeouts = new Set<ReturnType<typeof setTimeout>>();
		// T-2: fires at most once per mount, off the first fit() that succeeds.
		let attachNudged = false;
		// T-2: set between the nudge's two steps. Run by the rAF normally, or
		// synchronously by the unmount cleanup if we tear down mid-wobble.
		let pendingNudgeRestore: (() => void) | null = null;

		// Only attach-mode mounts with a stable session id participate in the
		// module-scope cache. Spawn-mode (one-off terminals) and detached
		// windows (no `sessionId` passed) always get a fresh terminal, exactly
		// as before.
		const cacheable = Boolean(sessionId && pty);
		const cachedEntry = cacheable ? xtermCache.get(sessionId as string) : undefined;

		// Fit-retry loop shared by both the cache-hit and cache-miss paths —
		// re-declared per mount since it references this mount's `disposed`/
		// `pendingRafs` (created above) and the resolved `term`/`fit` below.
		let term: Terminal;
		let fit: FitAddon;
		let container: HTMLDivElement;

		// T-2 (SIGWINCH-on-attach): a two-step resize wobble, run once against
		// `pty` directly after the terminal's first real fit(). This is NOT
		// defensive padding — it is the only thing that makes a full-screen TUI
		// (vim, htop, claude) repaint at the popped-out window's actual size.
		//
		// Why it's needed: the Linux tty layer's `tty_do_resize` memcmps the
		// incoming winsize against the current one and silently drops the
		// SIGWINCH when they're equal (`portable-pty`'s `master.resize()` issues
		// TIOCSWINSZ unconditionally — nothing upstream of the kernel coalesces
		// same-size resizes, the kernel itself does). If this window's fitted
		// size happens to match whatever size the PTY was already at, a plain
		// `pty.resize(rows, cols)` is a no-op from the child process's point of
		// view and the TUI never learns the window changed.
		//
		// Why two DIFFERENT sizes: resizing to (rows, cols-1) then, next frame,
		// back to (rows, cols) guarantees the second call is never a repeat of
		// the PTY's current winsize, so the kernel can't drop it.
		//
		// Why `pty.resize` and never `term.resize`: xterm's own `Terminal.
		// resize()` early-returns when rows/cols are unchanged and would never
		// fire `onResize` for the wobble-back step — routing through it would
		// silently produce zero SIGWINCHes. Only the child process should ever
		// see the wobble; the local grid must not flicker, so we call the PTY
		// bridge directly and never touch `term`'s own size.
		const scheduleAttachNudge = () => {
			if (attachNudged || !pty) return;
			attachNudged = true;
			const rows = term.rows;
			// Both steps must survive Rust's `rows.max(1)` / `cols.max(1)` clamp
			// as DIFFERENT values, so normalise to the post-clamp floor first.
			// Taking the raw `term.cols` here would degenerate at cols===0 —
			// wobble 1, restore 0→clamped-to-1 — leaving both steps identical
			// and the kernel dropping the SIGWINCH, i.e. exactly the silent
			// no-op this whole function exists to prevent.
			const cols = Math.max(1, term.cols);
			// At cols<=1 (a fresh detached webview can measure ~1 column before
			// fonts/layout settle — see terminal-surface.tsx) wobble UP instead
			// of down, or both steps would clamp to the same size in Rust.
			const wobbleCols = cols <= 1 ? cols + 1 : cols - 1;
			pty.resize(rows, wobbleCols).catch(() => {});
			// Step 1 has now shrunk a PTY that is SHARED with the origin pane and
			// outlives this window. The restore below MUST still happen if we
			// unmount in the ~16ms before the frame lands: the unmount cleanup
			// cancels every pending rAF, so relying on the rAF alone would strand
			// the shared PTY at `wobbleCols` permanently — the origin pane
			// remounts but only re-emits a resize when its own container geometry
			// changes, so nothing would ever put it back. Park the restore where
			// the cleanup can run it synchronously, and null it out once done so
			// it can never fire twice.
			pendingNudgeRestore = () => {
				pendingNudgeRestore = null;
				pty.resize(rows, cols).catch(() => {});
			};
			const id = requestAnimationFrame(() => {
				pendingRafs.delete(id);
				pendingNudgeRestore?.();
			});
			pendingRafs.add(id);
		};

		const queueFit = (attempt = 0) => {
			const id = requestAnimationFrame(() => {
				pendingRafs.delete(id);
				if (disposed) return;
				if (!term.element || !term.element.isConnected) return;
				try {
					fit.fit();
					if (nudgeOnAttach) scheduleAttachNudge();
				} catch {
					// Renderer not ready. Back off and retry: 16ms, 32ms, 64ms,
					// 128ms, 256ms — covers the WebGL init window on slow
					// machines without busy-looping. Give up after 5 tries;
					// ResizeObserver will still catch any later container-size
					// change.
					if (attempt >= 5) return;
					const delay = 16 << attempt;
					const t = setTimeout(() => {
						pendingTimeouts.delete(t);
						if (disposed) return;
						queueFit(attempt + 1);
					}, delay);
					pendingTimeouts.add(t);
				}
			});
			pendingRafs.add(id);
		};

		// Non-cached teardown state (spawn-mode ownedPty, and everything a
		// fresh non-cached mount creates that must be fully disposed on
		// unmount rather than handed to the cache).
		let ownedPty: Pty | null = null;
		let webglAddon: WebglAddon | null = null;
		let webglUsed = false;
		let disposeSearch: (() => void) | null = null;
		let pathLinksDisposeFn: (() => void) | null = null;
		let detachData: (() => void) | null = null;
		let detachExit: (() => void) | null = null;
		let onDataDispose: { dispose: () => void } | null = null;
		let onResizeDispose: { dispose: () => void } | null = null;
		let oscObserver: ReturnType<typeof createOscObserver>;

		if (cachedEntry) {
			// --- CACHE HIT: reuse the existing Terminal + container. ---
			term = cachedEntry.term;
			fit = cachedEntry.fit;
			container = cachedEntry.container;
			webglAddon = cachedEntry.webglAddon;
			webglUsed = cachedEntry.webglUsed;
			oscObserver = cachedEntry.oscObserver;

			termRef.current = term;
			searchAddonRef.current = cachedEntry.searchAddon;
			// The file-drop handler writes to `livePtyRef`. On a cache hit the
			// spawn/attach wiring below is skipped, so set it here too —
			// otherwise a terminal that has been remounted (tab switch, pane
			// move: the common case) has a null ref and silently drops the path.
			if (pty) livePtyRef.current = pty;

			if (container.parentElement !== mountEl) {
				mountEl.appendChild(container);
			}

			// The underlying PTY changed identity since this entry was cached
			// (a restart minted a new process for the same session) — detach
			// the old listeners and rewire against the current one exactly
			// once, rather than leaving the terminal driving a dead PTY or
			// double-attaching to the new one.
			if (pty && cachedEntry.wiredPtyId !== pty.id) {
				cachedEntry.detachData();
				cachedEntry.detachExit();
				cachedEntry.onDataDispose.dispose();
				cachedEntry.onResizeDispose.dispose();
				const wired = wirePtyToTerm(term, pty, oscObserver, status, exit, webglUsed);
				cachedEntry.wiredPtyId = pty.id;
				cachedEntry.detachData = wired.detachData;
				cachedEntry.detachExit = wired.detachExit;
				cachedEntry.onDataDispose = wired.onDataDispose;
				cachedEntry.onResizeDispose = wired.onResizeDispose;
			}
		} else {
			// --- CACHE MISS (or non-cacheable spec/detached mode): build fresh. ---
			const dark = isDarkMode();
			term = new Terminal({
				fontFamily:
					'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace',
				fontSize: 13,
				lineHeight: 1.2,
				cursorBlink: true,
				cursorStyle: 'block',
				scrollback: scrollback ?? 10_000,
				allowProposedApi: true,
				theme: readThemeFromCssVars(dark),
				macOptionIsMeta: true,
				convertEol: false,
				linkHandler: {
					activate: (_e: MouseEvent, text: string) => {
						if (/^[a-z]+:\/\//i.test(text) && !text.startsWith('file://')) {
							window.open(text, '_blank');
							return;
						}
						let filePath = text;
						let line: number | undefined;
						let col: number | undefined;
						filePath = fileUrlToPath(filePath);
						const hashMatch = filePath.match(/#L?(\d+)(?::(?:C|col)?(\d+))?$/i);
						if (hashMatch) {
							line = parseInt(hashMatch[1], 10);
							if (hashMatch[2]) col = parseInt(hashMatch[2], 10);
							filePath = filePath.slice(0, filePath.length - hashMatch[0].length);
						} else {
							const colonMatch = filePath.match(/:(\d+)(?::(\d+))?$/);
							if (colonMatch) {
								line = parseInt(colonMatch[1], 10);
								if (colonMatch[2]) col = parseInt(colonMatch[2], 10);
								filePath = filePath.slice(0, filePath.length - colonMatch[0].length);
							}
						}
						const effectiveCwd = livePtyRef.current?.cwd ?? spec?.cwd;
						const resolved = resolvePath(filePath, effectiveCwd);
						const store = usePaneStore.getState();
						store.addTabBackground(store.focusedId, {
							kind: 'artifact',
							path: resolved,
							line,
							col,
						});
					},
				},
			});
			termRef.current = term;

			fit = new FitAddon();
			const links = new WebLinksAddon();
			const unicode11 = new Unicode11Addon();
			term.loadAddon(fit);
			term.loadAddon(links);
			term.loadAddon(unicode11);
			term.unicode.activeVersion = '11';

			// File-path links (WebLinksAddon only handles URLs). Clicking a
			// path-shaped token opens it in the artifact viewer. Relative paths
			// resolve against the live PTY / foreground cwd; absolute / ~ paths ignore it
			// (works in attach-mode too, where no cwd is known).
			const pathLinks = registerPathLinks(term, () => livePtyRef.current?.cwd ?? spec?.cwd);
			pathLinksDisposeFn = () => pathLinks.dispose();

			// OSC 7: Current Working Directory notification emitted by shells (T-10)
			// (e.g. `\x1b]7;file://hostname/path\x1b\\` or `\x1b]7;/path\x1b\\`).
			term.parser.registerOscHandler(7, (data) => {
				try {
					const path = fileUrlToPath(data);
					if (path) {
						if (livePtyRef.current) livePtyRef.current.setCwd(path);
						if (sessionId) useTerminalStore.getState().updateCwd?.(sessionId, path);
					}
				} catch {
					/* ignore malformed OSC 7 */
				}
				return true;
			});

			// OSC 133: FinalTerm / Shell Integration property markers (T-10)
			// (e.g. `133;P;Cwd=/path`).
			term.parser.registerOscHandler(133, (data) => {
				try {
					const parts = data.split(';');
					const code = parts[0];
					if (code === 'P' && parts[1]?.startsWith('Cwd=')) {
						const cwdVal = parts[1].slice(4).trim();
						if (cwdVal) {
							if (livePtyRef.current) livePtyRef.current.setCwd(cwdVal);
							if (sessionId) useTerminalStore.getState().updateCwd?.(sessionId, cwdVal);
						}
					}
				} catch {
					/* ignore malformed OSC 133 */
				}
				return true;
			});

			// Search addon — lazy import to keep initial bundle slim.
			(async () => {
				try {
					const mod = await import('@xterm/addon-search');
					if (cancelled) return;
					const search = new mod.SearchAddon();
					term.loadAddon(search);
					let resultsDispose: (() => void) | null = null;
					if (search.onDidChangeResults) {
						const sub = search.onDidChangeResults((e: SearchResultChangeEvent) => {
							setSearchResult(e);
						});
						resultsDispose = () => sub.dispose();
					}
					const searchLike: SearchAddonLike = {
						findNext: (s, opts) => search.findNext(s, opts),
						findPrevious: (s, opts) => search.findPrevious(s, opts),
						onDidChangeResults: search.onDidChangeResults
							? (l) => search.onDidChangeResults(l)
							: undefined,
						clearDecorations: () => search.clearDecorations(),
						dispose: () => {
							resultsDispose?.();
							search.dispose();
						},
					};
					searchAddonRef.current = searchLike;
					disposeSearch = () => {
						resultsDispose?.();
						search.dispose();
					};
					const entry = cacheable ? xtermCache.get(sessionId as string) : undefined;
					if (entry) entry.searchAddon = searchLike;
				} catch (err) {
					console.warn('[xterm] search addon failed to load', err);
				}
			})();

			// Cacheable (attach-mode + sessionId) mounts open into a nested div
			// so it can be re-parented on a later remount; non-cacheable mounts
			// (spec-mode, detached windows) open straight into the ref'd host
			// div, matching the pre-cache behavior exactly.
			if (cacheable) {
				container = document.createElement('div');
				container.style.width = '100%';
				container.style.height = '100%';
				mountEl.appendChild(container);
			} else {
				container = mountEl;
			}
			term.open(container);
			term.textarea?.setAttribute('aria-label', 'Terminal');
			term.element?.setAttribute('aria-label', 'Terminal');

			if (disableWebgl) {
				status('dom renderer (webgl disabled)');
			} else {
				try {
					webglAddon = new WebglAddon();
					webglAddon.onContextLoss(() => {
						webglAddon?.dispose();
						webglAddon = null;
						status('webgl context lost — fell back to dom renderer');
						const entry = cacheable ? xtermCache.get(sessionId as string) : undefined;
						if (entry) entry.webglAddon = null;
					});
					term.loadAddon(webglAddon);
					webglUsed = true;
				} catch (err) {
					console.warn('[xterm] webgl addon failed, using dom fallback', err);
					status('webgl unavailable — dom renderer');
				}
			}

			oscObserver = createOscObserver({ onNotify: (n) => void fireOscNotification(n) });

			// Ring-replay fallback: attaching to a session whose PTY has been
			// alive without a cached xterm listening (cache was evicted, or
			// this is the first reclaim after a pop-out) — replay whatever the
			// per-session capture ring (pty-output-buffer.ts, wired once at
			// spawn in single-terminal.tsx) still holds before wiring the live
			// stream, so scrollback survives the gap. Mirrors the detached-
			// window `Pty.attach` scrollback replay (pty-bridge.ts), including
			// its offset reconciliation: the ring snapshot is tagged with an
			// absolute stream offset, and `primeExternalSnapshot` drops any
			// buffered/live bytes at or below that offset so the seam is not
			// double-painted. Must run before `wirePtyToTerm` attaches the live
			// `onData` subscriber (so a buffered replay is trimmed first).
			if (cacheable && sessionId && pty) {
				const snap = readCaptureWithOffset(sessionId);
				if (snap && snap.data.length > 0) {
					try {
						term.write(snap.data);
					} catch {
						/* ignore */
					}
					pty.primeExternalSnapshot(snap.endOffset);
				}
			}

			if (pty) {
				livePtyRef.current = pty;
				const wired = wirePtyToTerm(term, pty, oscObserver, status, exit, webglUsed);
				detachData = wired.detachData;
				detachExit = wired.detachExit;
				onDataDispose = wired.onDataDispose;
				onResizeDispose = wired.onResizeDispose;

				if (cacheable && sessionId) {
					xtermCache.set(sessionId, {
						term,
						container,
						fit,
						webglAddon,
						webglUsed,
						searchAddon: searchAddonRef.current,
						// Non-null: `pathLinksDisposeFn` is always assigned earlier
						// in this same (cache-miss) branch, before any code path
						// that can reach here.
						pathLinksDispose: pathLinksDisposeFn as () => void,
						oscObserver,
						wiredPtyId: pty.id,
						detachData,
						detachExit,
						onDataDispose,
						onResizeDispose,
					});
				}
			}
		}

		// --- Per-mount rebindings (always run, cache hit or miss). ---

		// Copy/paste + interrupt key handling. `attachCustomKeyEventHandler`
		// replaces any previously-registered handler wholesale, so re-running
		// this every mount is required (not a double-attach) — it rebinds the
		// closure to THIS mount's `setSearchOpen`/`searchInputRef`, which a
		// cache-hit reparent would otherwise leave pointed at a dead instance.
		term.attachCustomKeyEventHandler((e) => {
			if (e.type !== 'keydown') return true;
			const mac = isMac();
			const meta = mac ? e.metaKey : e.ctrlKey;

			const action = evaluateTerminalKey(e, mac, keybindingsRef.current);
			if (action === 'copy') {
				const sel = term.getSelection();
				if (sel) {
					navigator.clipboard.writeText(sel).catch(() => {});
					return false;
				}
				// On Mac with Cmd+C, if no selection, fall through to PTY (SIGINT).
				if (mac) return true;
				return false;
			}

			if (action === 'paste') {
				navigator.clipboard
					.readText()
					.then((t) => term.paste(t))
					.catch(() => {});
				return false;
			}

			if (action === 'find') {
				setSearchOpen(true);
				setTimeout(() => searchInputRef.current?.focus(), 0);
				return false;
			}

			if (action === 'clear') {
				term.clear();
				return false;
			}

			if (action === 'selectAll') {
				term.selectAll();
				return false;
			}

			// Zoom chords belong to the app, not the PTY. xterm's handler runs
			// on its own textarea and therefore *before* the window-level zoom
			// listener in `lib/window/zoom.ts`; without this, Ctrl+- and
			// friends get encoded and shipped to the shell running inside the
			// terminal. Returning false only stops xterm from consuming the
			// key — the event still bubbles to window, where zoom picks it up.
			if (meta && !e.altKey && ['=', '+', 'Add', '-', '_', 'Subtract', '0'].includes(e.key)) {
				return false;
			}

			// Shift+Enter — soft newline instead of submit.
			//
			// xterm sends CR (\r, 0x0d) for Enter. A bare terminal has no way to
			// distinguish Shift+Enter, so it sends CR for that too and the app
			// submits — which is why multi-line input in the claude CLI (and
			// other TUIs that accept it) doesn't work in an unconfigured
			// terminal. Sending LF (\n, 0x0a) here gives the app a second,
			// distinguishable key: readline-style consumers treat it as a
			// literal newline in the buffer rather than end-of-input. This is
			// the same distinction `/terminal-setup` configures in iTerm2 and
			// VS Code.
			//
			// `term.input()` (not `pty.write()`) so this routes through the
			// terminal's own onData path — that keeps it correct on the
			// cache-hit remount, where the enclosing closure's `pty` may be a
			// stale handle from a previous mount.
			// `preventDefault()` is load-bearing and NOT implied by `return
			// false`: returning false only tells xterm to skip its own
			// processing — it leaves the browser's default action intact, so
			// the keystroke still reached the textarea and xterm sent CR right
			// behind our LF. The visible symptom was a newline appearing and
			// the app submitting a frame later. Suppress the default so LF is
			// the only thing that reaches the PTY.
			if (e.shiftKey && !e.ctrlKey && !e.metaKey && !e.altKey && e.key === 'Enter') {
				e.preventDefault();
				term.input('\n');
				return false;
			}

			// Plain Ctrl+C on linux still goes to PTY (xterm default — SIGINT).
			// No special handling needed; meta-only branch above is mac-only.
			void meta;
			return true;
		});

		// Theme sync — observe <html class="dark"> changes and CSS-var
		// updates. Recreated every mount (cheap, idempotent) rather than
		// cached, so it always reflects the live component tree.
		const themeObserver = new MutationObserver(() => {
			if (disposed) return;
			try {
				term.options.theme = readThemeFromCssVars(isDarkMode());
			} catch {
				/* ignore */
			}
		});
		themeObserver.observe(document.documentElement, {
			attributes: true,
			attributeFilter: ['class', 'style', 'data-theme'],
		});

		// Defer fit() until after the renderer's first paint (see queueFit's
		// backoff comment above) — matters just as much on a cache-hit
		// re-parent, since the container just moved to a new DOM position.
		queueFit();

		let resizeObserver: ResizeObserver | null = new ResizeObserver(() => {
			if (disposed) return;
			queueFit();
		});
		resizeObserver.observe(container);

		if (cachedEntry) {
			// Re-parented an already-live terminal — only steal focus if the
			// hosting pane is actually the focused one.
			if (focusedRef.current) term.focus();
		} else if (pty) {
			// Fresh attach-mode mount — always focus, matching prior behavior.
			term.focus();
		}

		// Spawn-mode (non-cacheable) lifecycle: create a new PTY tied to this
		// component. Runs after the synchronous setup above so `queueFit` /
		// `resizeObserver` / focus logic is shared; spawn-mode never hits the
		// cache (`cacheable` is false whenever `spec` is used instead of `pty`).
		if (!pty && spec) {
			(async () => {
				try {
					const opts: PtySpawnOpts = {
						cwd: spec.cwd,
						cmd: spec.cmd,
						env: spec.env,
						rows: term.rows,
						cols: term.cols,
						label: spec.cmd.join(' '),
					};
					const p = await Pty.spawn(opts);
					if (cancelled) {
						await p.dispose().catch(() => {});
						return;
					}
					ownedPty = p;
					livePtyRef.current = p;
					onPtyIdRef.current?.(p.id);
					const wired = wirePtyToTerm(term, p, oscObserver, status, exit, webglUsed);
					detachData = wired.detachData;
					detachExit = wired.detachExit;
					onDataDispose = wired.onDataDispose;
					onResizeDispose = wired.onResizeDispose;
					term.focus();
				} catch (err) {
					const msg = err instanceof Error ? err.message : String(err);
					term.write(`\r\n[spawn failed] ${msg}\r\n`);
					status(`spawn failed: ${msg}`);
				}
			})();
		}

		return () => {
			cancelled = true;
			disposed = true;
			livePtyRef.current = null;
			// Cancel any in-flight rAFs + retry timeouts so they can't run fit()
			// after dispose.
			for (const id of pendingRafs) cancelAnimationFrame(id);
			pendingRafs.clear();
			// T-2: if we tore down mid-wobble, the rAF that would have restored
			// the shared PTY's real width was just cancelled above. Run it here
			// instead — otherwise the PTY stays one column narrow for the rest of
			// its life. Safe after dispose: `Pty.resize` no-ops once the PTY
			// itself is disposed/exited.
			pendingNudgeRestore?.();
			for (const t of pendingTimeouts) clearTimeout(t);
			pendingTimeouts.clear();
			themeObserver.disconnect();
			resizeObserver?.disconnect();
			resizeObserver = null;

			if (cacheable) {
				// Cached path: the terminal, its addons, and its PTY listeners
				// are owned by the module-scope cache now, not this mount.
				// Deliberately do NOT dispose them here — that's what made
				// every remount look like a restart. Eviction happens via the
				// terminal-store subscription (session actually closed) or the
				// HMR guard above, not on a plain unmount.
				return;
			}

			// Non-cached path (spec-mode / detached-window attach): tear
			// everything down exactly as before.
			onDataDispose?.dispose();
			onResizeDispose?.dispose();
			detachData?.();
			detachExit?.();
			disposeSearch?.();
			pathLinksDisposeFn?.();
			// Only kill the PTY if we own it (spawn-mode). In attach-mode the
			// session-store (or detached-window caller) owns the lifecycle.
			if (ownedPty) {
				ownedPty.dispose().catch(() => {});
			}
			// Null the ref BEFORE disposing so any late callback bails on the
			// null check rather than touching a half-torn-down renderer.
			termRef.current = null;
			try {
				webglAddon?.dispose();
			} catch {
				/* ignore */
			}
			try {
				term.dispose();
			} catch {
				/* xterm sometimes throws when renderer is mid-frame; safe to drop */
			}
		};
		// Re-fire when the underlying source changes. Callbacks live in refs;
		// `focused` is read via `focusedRef` so a pane-focus flip alone
		// doesn't tear down and re-run this whole mount effect.
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, [spec, pty, sessionId]);

	const getSearchOptions = (optOverrides?: {
		caseSensitive?: boolean;
		wholeWord?: boolean;
		regex?: boolean;
	}): SearchOptions => ({
		caseSensitive: optOverrides?.caseSensitive ?? caseSensitive,
		wholeWord: optOverrides?.wholeWord ?? wholeWord,
		regex: optOverrides?.regex ?? useRegex,
		incremental: true,
		decorations: {
			matchBackground: isDarkMode() ? '#515c6a' : '#fed7aa',
			matchOverviewRuler: '#f59e0b',
			activeMatchBackground: isDarkMode() ? '#3b82f6' : '#60a5fa',
			activeMatchColorOverviewRuler: '#2563eb',
		},
	});

	function runSearch(
		direction: 'next' | 'prev',
		termOverride?: string,
		optOverrides?: { caseSensitive?: boolean; wholeWord?: boolean; regex?: boolean }
	) {
		const addon = searchAddonRef.current;
		const term = termOverride ?? searchTerm;
		if (!addon || !term) {
			setSearchResult(null);
			return;
		}
		const opts = getSearchOptions(optOverrides);
		if (direction === 'next') addon.findNext(term, opts);
		else addon.findPrevious(term, opts);
	}

	// Drop a file onto the terminal → insert its shell-quoted path (trailing
	// space, no newline, so the user reviews before running).
	//
	// The path comes from the native OS drag-drop handler, re-dispatched to the
	// element under the cursor as `OS_FILE_DROP_EVENT` by the global router
	// (src/lib/dnd/os-file-drop.ts). We can't read it from an HTML5 drop:
	// WebKitGTK blanks `dataTransfer` for security when the native handler is on,
	// and the native handler is what carries the real path. Inserting the path
	// needs no file read, so this works for any file regardless of the fs
	// allowlist. On macOS (native handler disabled) this event never fires and
	// terminal path-drop is unavailable — documented in lib.rs.
	//
	// MUST stay above the `!spec && !pty` early return below: a hook after a
	// conditional return changes the hook count between renders, and React
	// throws "rendered more hooks than during the previous render" the first
	// time a pane flips between having a terminal and not.
	useEffect(() => {
		const el = wrapperRef.current;
		if (!el) return;
		const onPaths = (e: Event) => {
			const detail = (e as CustomEvent<OsFileDropDetail>).detail;
			if (!detail?.paths?.length) return;
			e.stopPropagation();
			const pty = livePtyRef.current;
			if (!pty) return;
			pty.write(`${detail.paths.map(shellQuote).join(' ')} `).catch(console.error);
			termRef.current?.focus();
		};
		el.addEventListener(OS_FILE_DROP_EVENT, onPaths);
		return () => el.removeEventListener(OS_FILE_DROP_EVENT, onPaths);
	}, []);

	const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);

	useEffect(() => {
		if (!contextMenu) return;
		const close = () => setContextMenu(null);
		const onKey = (e: KeyboardEvent) => {
			if (e.key === 'Escape') setContextMenu(null);
		};
		window.addEventListener('click', close);
		window.addEventListener('contextmenu', close);
		window.addEventListener('keydown', onKey);
		return () => {
			window.removeEventListener('click', close);
			window.removeEventListener('contextmenu', close);
			window.removeEventListener('keydown', onKey);
		};
	}, [contextMenu]);

	const handleContextMenu = (e: React.MouseEvent) => {
		e.preventDefault();
		setContextMenu({ x: e.clientX, y: e.clientY });
	};

	const effectiveKeybindings = {
		...getDefaultKeybindings(isMac()),
		...keybindings,
	};

	if (!spec && !pty) {
		return <div className="empty">No PTY. Spawn one above.</div>;
	}

	return (
		<div
			ref={wrapperRef}
			data-terminal-session={sessionId}
			onContextMenu={handleContextMenu}
			style={{
				position: 'relative',
				width: '100%',
				height: '100%',
				display: 'flex',
				flexDirection: 'column',
			}}
		>
			{contextMenu && (
				<div
					role="menu"
					aria-label="Terminal Context Menu"
					style={{
						position: 'fixed',
						top: contextMenu.y,
						left: contextMenu.x,
						zIndex: 9999,
						minWidth: 160,
						padding: '4px',
						background: 'var(--color-card)',
						border: '1px solid var(--color-border)',
						borderRadius: 6,
						boxShadow: '0 6px 20px rgba(0,0,0,0.4)',
						fontSize: 12,
						color: 'var(--color-card-foreground)',
						display: 'flex',
						flexDirection: 'column',
						gap: 2,
					}}
					onClick={(e) => e.stopPropagation()}
				>
					<button
						type="button"
						disabled={!termRef.current?.hasSelection()}
						onClick={() => {
							const sel = termRef.current?.getSelection();
							if (sel) navigator.clipboard.writeText(sel).catch(() => {});
							setContextMenu(null);
						}}
						style={{
							display: 'flex',
							alignItems: 'center',
							justifyContent: 'space-between',
							padding: '5px 8px',
							border: 'none',
							borderRadius: 4,
							background: 'transparent',
							color: termRef.current?.hasSelection() ? 'inherit' : 'rgba(127,127,127,0.5)',
							cursor: termRef.current?.hasSelection() ? 'pointer' : 'default',
							textAlign: 'left',
						}}
					>
						<span>Copy</span>
						<span style={{ fontSize: 10, opacity: 0.6 }}>{effectiveKeybindings.copy}</span>
					</button>
					<button
						type="button"
						onClick={() => {
							navigator.clipboard
								.readText()
								.then((t) => {
									if (t) {
										livePtyRef.current?.write(t).catch(() => {});
										termRef.current?.focus();
									}
								})
								.catch(() => {});
							setContextMenu(null);
						}}
						style={{
							display: 'flex',
							alignItems: 'center',
							justifyContent: 'space-between',
							padding: '5px 8px',
							border: 'none',
							borderRadius: 4,
							background: 'transparent',
							color: 'inherit',
							cursor: 'pointer',
							textAlign: 'left',
						}}
					>
						<span>Paste</span>
						<span style={{ fontSize: 10, opacity: 0.6 }}>{effectiveKeybindings.paste}</span>
					</button>
					<button
						type="button"
						onClick={() => {
							termRef.current?.selectAll();
							setContextMenu(null);
						}}
						style={{
							display: 'flex',
							alignItems: 'center',
							justifyContent: 'space-between',
							padding: '5px 8px',
							border: 'none',
							borderRadius: 4,
							background: 'transparent',
							color: 'inherit',
							cursor: 'pointer',
							textAlign: 'left',
						}}
					>
						<span>Select All</span>
						<span style={{ fontSize: 10, opacity: 0.6 }}>{effectiveKeybindings.selectAll}</span>
					</button>
					<div style={{ height: 1, background: 'rgba(127,127,127,0.2)', margin: '2px 0' }} />
					<button
						type="button"
						onClick={() => {
							termRef.current?.clear();
							setContextMenu(null);
						}}
						style={{
							display: 'flex',
							alignItems: 'center',
							justifyContent: 'space-between',
							padding: '5px 8px',
							border: 'none',
							borderRadius: 4,
							background: 'transparent',
							color: 'inherit',
							cursor: 'pointer',
							textAlign: 'left',
						}}
					>
						<span>Clear Terminal</span>
						<span style={{ fontSize: 10, opacity: 0.6 }}>{effectiveKeybindings.clear}</span>
					</button>
					<button
						type="button"
						onClick={() => {
							setSearchOpen(true);
							setTimeout(() => searchInputRef.current?.focus(), 0);
							setContextMenu(null);
						}}
						style={{
							display: 'flex',
							alignItems: 'center',
							justifyContent: 'space-between',
							padding: '5px 8px',
							border: 'none',
							borderRadius: 4,
							background: 'transparent',
							color: 'inherit',
							cursor: 'pointer',
							textAlign: 'left',
						}}
					>
						<span>Find…</span>
						<span style={{ fontSize: 10, opacity: 0.6 }}>{effectiveKeybindings.find}</span>
					</button>
				</div>
			)}
			{searchOpen && (
				<div
					style={{
						display: 'flex',
						alignItems: 'center',
						gap: 4,
						padding: '4px 6px',
						borderBottom: '1px solid rgba(127,127,127,0.2)',
						background: 'rgba(127,127,127,0.06)',
						fontSize: 12,
					}}
				>
					<input
						ref={searchInputRef}
						value={searchTerm}
						onChange={(e) => {
							const val = e.target.value;
							setSearchTerm(val);
							if (val) runSearch('next', val);
							else setSearchResult(null);
						}}
						onKeyDown={(e) => {
							if (e.key === 'Enter') {
								runSearch(e.shiftKey ? 'prev' : 'next');
							} else if (e.key === 'Escape') {
								setSearchOpen(false);
								searchAddonRef.current?.clearDecorations?.();
								termRef.current?.focus();
							}
						}}
						placeholder="Search…"
						style={{
							flex: 1,
							padding: '2px 6px',
							border: '1px solid rgba(127,127,127,0.3)',
							borderRadius: 3,
							background: 'transparent',
							color: 'inherit',
							fontSize: 12,
							outline: 'none',
						}}
					/>
					{searchTerm && searchResult && (
						<span style={{ fontSize: 10, opacity: 0.7, padding: '0 4px', whiteSpace: 'nowrap' }}>
							{searchResult.resultCount === 0
								? 'No results'
								: `${searchResult.resultIndex >= 0 ? searchResult.resultIndex + 1 : '?'} of ${searchResult.resultCount}`}
						</span>
					)}
					<button
						type="button"
						onClick={() => {
							const next = !caseSensitive;
							setCaseSensitive(next);
							if (searchTerm) runSearch('next', searchTerm, { caseSensitive: next });
						}}
						style={{
							fontSize: 11,
							padding: '1px 5px',
							borderRadius: 3,
							border: '1px solid',
							borderColor: caseSensitive ? 'rgba(59, 130, 246, 0.6)' : 'rgba(127,127,127,0.3)',
							background: caseSensitive ? 'rgba(59, 130, 246, 0.2)' : 'transparent',
							color: 'inherit',
							cursor: 'pointer',
							fontFamily: 'monospace',
						}}
						title="Match Case (Aa)"
					>
						Aa
					</button>
					<button
						type="button"
						onClick={() => {
							const next = !wholeWord;
							setWholeWord(next);
							if (searchTerm) runSearch('next', searchTerm, { wholeWord: next });
						}}
						style={{
							fontSize: 11,
							padding: '1px 5px',
							borderRadius: 3,
							border: '1px solid',
							borderColor: wholeWord ? 'rgba(59, 130, 246, 0.6)' : 'rgba(127,127,127,0.3)',
							background: wholeWord ? 'rgba(59, 130, 246, 0.2)' : 'transparent',
							color: 'inherit',
							cursor: 'pointer',
							fontFamily: 'monospace',
						}}
						title="Match Whole Word (\b)"
					>
						\b
					</button>
					<button
						type="button"
						onClick={() => {
							const next = !useRegex;
							setUseRegex(next);
							if (searchTerm) runSearch('next', searchTerm, { regex: next });
						}}
						style={{
							fontSize: 11,
							padding: '1px 5px',
							borderRadius: 3,
							border: '1px solid',
							borderColor: useRegex ? 'rgba(59, 130, 246, 0.6)' : 'rgba(127,127,127,0.3)',
							background: useRegex ? 'rgba(59, 130, 246, 0.2)' : 'transparent',
							color: 'inherit',
							cursor: 'pointer',
							fontFamily: 'monospace',
						}}
						title="Use Regular Expression (.*)"
					>
						.*
					</button>
					<button
						type="button"
						onClick={() => runSearch('prev')}
						style={{ fontSize: 11, padding: '1px 6px', cursor: 'pointer' }}
						title="Previous (Shift+Enter)"
					>
						↑
					</button>
					<button
						type="button"
						onClick={() => runSearch('next')}
						style={{ fontSize: 11, padding: '1px 6px', cursor: 'pointer' }}
						title="Next (Enter)"
					>
						↓
					</button>
					<button
						type="button"
						onClick={() => {
							setSearchOpen(false);
							searchAddonRef.current?.clearDecorations?.();
							termRef.current?.focus();
						}}
						style={{ fontSize: 11, padding: '1px 6px', cursor: 'pointer' }}
						title="Close (Esc)"
					>
						×
					</button>
				</div>
			)}
			<div ref={containerRef} className="terminal-host" style={{ flex: 1, minHeight: 0 }} />
		</div>
	);
}
