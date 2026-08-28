// Ikenga artifact bridge — host-injected runtime polyfill.
//
// What this is:
//   The viewer-server (`src-tauri/src/viewer_server/mod.rs`) injects the
//   bundled output of this module into every served Ikenga artifact's
//   <head>. It populates two window globals before the artifact's own
//   inline polyfill (if any) runs:
//
//     window.__ikenga_host__         — host descriptor; presence signals
//                                      "you're running inside Ikenga".
//     window.__ikenga_bridge_polyfill__.init()
//                                    — returns a Promise<Art> that the
//                                      artifact's React code awaits.
//
// Shape contract: mirrors the inline polyfill in
//   ikenga-artifact-builder/skills/ikenga-artifact-builder/references/hello-world.html
// expanded to cover the full surface in that skill's SKILL.md
// ("Bridge surface (cheat sheet)" section).
//
// v0 scope: fetch + file sources do real reads with mock fallback;
// supabase/sql/mcp sources resolve directly to mock; notes/pin are
// console stubs. Phase 2 replaces the non-fetch resolvers with host RPC.
//
// Theme: `setupTheme()` mirrors the shell's `data-mode`/`data-theme`/
// `data-density` (+ a `.dark` class) onto the artifact's own `<html>` and
// re-applies live on every toggle; standalone it follows `prefers-color-
// scheme`. Exposed to artifact code as `art.theme`. The inline polyfill in
// the skill template carries only the standalone half (OS color scheme).
//
// Constraints (do not violate without updating bridge.entry.ts comment):
//   - Pure browser-side. No Node/Tauri imports. Runs in an iframe.
//   - No top-level await — must survive Babel-standalone compilation.
//   - Strict TypeScript.
//   - External imports are fine: `bun run artifact:bundle` inlines them.

import { domToPng } from 'modern-screenshot';
import * as M from './bridge-messages';
import { deriveSelector } from './selector';

// ── Types (inline; mirrors @ikenga/contract manifest shape minimally) ────

type RefreshMode = 'manual' | 'interval' | 'watch';

interface RefreshConfig {
	mode?: RefreshMode;
	every?: string;
	onFocus?: boolean;
}

interface FetchSource {
	type: 'fetch';
	url: string;
	method?: string;
	headers?: Record<string, string>;
	refresh?: RefreshConfig;
}

interface FileSource {
	type: 'file';
	/** Path relative to the artifact document. Must stay inside the mount. */
	path: string;
	refresh?: RefreshConfig;
}

interface OtherSource {
	type: 'supabase' | 'sql' | 'mcp';
	refresh?: RefreshConfig;
	[key: string]: unknown;
}

type DataSource = FetchSource | FileSource | OtherSource;

interface Manifest {
	id: string;
	dataSources?: Record<string, DataSource>;
	[key: string]: unknown;
}

interface HostDescriptor {
	kind: 'ikenga' | 'browser';
	user: null;
}

interface SourceHandle {
	get: () => unknown;
	subscribe: (fn: (value: unknown) => void) => () => void;
	refresh: () => Promise<void>;
}

interface StateHandle {
	get: (key: string) => unknown;
	set: (key: string, value: unknown) => void;
	subscribe: (key: string, fn: (value: unknown) => void) => () => void;
}

interface NotesHandle {
	send: (text: string, opts?: Record<string, unknown>) => void;
}

type ThemeMode = 'light' | 'dark';

interface ThemeSnapshot {
	mode: ThemeMode;
	/** Palette variant — 'A' | 'B' | 'C' (or a custom theme id). */
	theme: string;
	density: string;
}

/**
 * Live view of the host theme. Reads track the current value; `subscribe`
 * fires on every shell toggle (or OS color-scheme change when standalone).
 */
interface ThemeHandle {
	readonly mode: ThemeMode;
	readonly theme: string;
	readonly density: string;
	applyFromHost: (payload: M.ThemePayload) => void;
	subscribe: (fn: (t: ThemeSnapshot) => void) => () => void;
}

interface Art {
	manifest: Manifest;
	host: {
		kind: 'ikenga' | 'browser';
		user: null;
		usedFallback: (name: string) => boolean;
		anyFallback: () => boolean;
	};
	source: (name: string) => SourceHandle;
	state: StateHandle;
	notes: NotesHandle;
	theme: ThemeHandle;
	pin: () => void;
}

interface BridgePolyfill {
	init: () => Promise<Art>;
}

declare global {
	interface Window {
		__ikenga_host__?: HostDescriptor;
		__ikenga_bridge_polyfill__?: BridgePolyfill;
	}
}

// ── Helpers ──────────────────────────────────────────────────────────────

function parseTagJson<T>(id: string): T | null {
	const el = document.getElementById(id);
	if (!el?.textContent) return null;
	try {
		return JSON.parse(el.textContent) as T;
	} catch {
		return null;
	}
}

/**
 * Parse a duration string like "30s", "15m", "1h", "2d" → ms.
 * Returns null if the string is malformed.
 */
function parseDuration(s: string | undefined): number | null {
	if (!s) return null;
	const match = /^(\d+)\s*([smhd])$/.exec(s.trim());
	if (!match) return null;
	const n = Number(match[1]);
	switch (match[2]) {
		case 's':
			return n * 1000;
		case 'm':
			return n * 60_000;
		case 'h':
			return n * 3_600_000;
		case 'd':
			return n * 86_400_000;
		default:
			return null;
	}
}

/**
 * Resolve a `file` source path against the artifact's own document URL.
 *
 * The viewer-server mounts the artifact's directory with `ServeDir` under
 * `/__viewer/<token>/`, so sibling data files are already reachable
 * same-origin — no Tauri/Node import required (this module is iframe-only).
 *
 * Returns null for anything that isn't a plain relative path staying inside
 * the mount: absolute paths, scheme-qualified URLs, protocol-relative URLs,
 * cross-origin results, and `../` traversal above the mount root. Callers
 * treat null as "fall back to mock" rather than fetching it anyway.
 */
function resolveArtifactRelative(path: string): string | null {
	if (typeof path !== 'string' || path.length === 0) return null;
	// Scheme-qualified (http:, file:, data:), protocol-relative, or rooted.
	if (/^[a-z][a-z0-9+.-]*:/i.test(path)) return null;
	if (path.startsWith('//') || path.startsWith('/')) return null;

	let resolved: URL;
	try {
		resolved = new URL(path, window.location.href);
	} catch {
		return null;
	}
	if (resolved.origin !== window.location.origin) return null;

	// Confine to the mount root when served by the viewer server; otherwise to
	// the document's own directory (standalone preview).
	const base = window.location.pathname;
	const mount = base.match(/^(\/__viewer\/[^/]+\/)/);
	const prefix = mount ? mount[1] : base.slice(0, base.lastIndexOf('/') + 1);
	if (!resolved.pathname.startsWith(prefix)) return null;

	return resolved.href;
}

// ── Theme mirroring ────────────────────────────────────────────────────────

/**
 * Make the artifact follow the host theme.
 *
 * The child iframe is sandboxed without `allow-same-origin`, so it cannot
 * read `window.parent.document`. The host posts a `theme` message whenever
 * the shell's `data-mode` / `data-theme` / `data-density` attributes change;
 * the child applies them to its own `<html>`. `@ikenga/tokens` is pre-injected
 * by the viewer server, so its `:root[data-mode=…]` / `[data-theme=…]`
 * selectors resolve once the attributes are present. We also toggle a `.dark`
 * class so Tailwind's class-strategy dark mode tracks the shell.
 *
 * Outside the shell (no theme message arrives) we fall back to the OS
 * `prefers-color-scheme` and follow it live; the palette defaults to 'A'.
 *
 * Runs synchronously in `<head>` (the bridge is the first, non-deferred
 * script), so the first paint is already themed — no flash.
 */
function setupTheme(): ThemeHandle {
	const subs: Array<(t: ThemeSnapshot) => void> = [];
	let current: ThemeSnapshot = { mode: 'dark', theme: 'A', density: 'comfortable' };

	function apply(next: ThemeSnapshot): void {
		current = next;
		const html = document.documentElement;
		html.setAttribute('data-mode', next.mode);
		html.setAttribute('data-theme', next.theme);
		html.setAttribute('data-density', next.density);
		html.classList.toggle('dark', next.mode === 'dark');
		for (const fn of subs.slice()) {
			try {
				fn(next);
			} catch (err) {
				console.error('[ikenga.theme] subscriber threw', err);
			}
		}
	}

	// The host bridge (setupHostBridge) calls this when it receives a
	// `theme` message. Exported on the returned handle for tests.
	function applyFromHost(payload: M.ThemePayload): void {
		apply({
			mode: payload.mode,
			theme: payload.theme,
			density: payload.density,
		});
	}

	// Standalone fallback: follow the OS color scheme. Palette has no host
	// source, so default to 'A'.
	const mql = window.matchMedia('(prefers-color-scheme: dark)');
	const fromOs = (): ThemeSnapshot => ({
		mode: mql.matches ? 'dark' : 'light',
		theme: 'A',
		density: 'comfortable',
	});

	apply(fromOs());
	const onChange = () => apply(fromOs());
	if (typeof mql.addEventListener === 'function') {
		mql.addEventListener('change', onChange);
	} else if (typeof mql.addListener === 'function') {
		// Safari < 14.
		mql.addListener(onChange);
	}

	return {
		get mode() {
			return current.mode;
		},
		get theme() {
			return current.theme;
		},
		get density() {
			return current.density;
		},
		applyFromHost,
		subscribe: (fn) => {
			subs.push(fn);
			return () => {
				const i = subs.indexOf(fn);
				if (i >= 0) subs.splice(i, 1);
			};
		},
	};
}

// ── Host postMessage bridge ──────────────────────────────────────────────

/** Serialise a `DOMRect` so it can be sent to the host over postMessage. */
function serialiseRect(r: DOMRect): M.SerializedRect {
	return { top: r.top, left: r.left, width: r.width, height: r.height };
}

function clamp01(v: number): number {
	if (!Number.isFinite(v)) return 0;
	if (v < 0) return 0;
	if (v > 1) return 1;
	return v;
}

function labelFor(el: Element): string {
	const tag = el.tagName.toLowerCase();
	const text = (el.textContent ?? '').trim().slice(0, 40);
	if (!text) return tag;
	return `${tag} — ${text}${(el.textContent ?? '').length > 40 ? '…' : ''}`;
}

function elementFromEventTarget(e: Event): Element | null {
	const t = e.target as { nodeType?: number } | null;
	if (!t || t.nodeType !== 1) return null;
	return e.target as Element;
}

function computePosition(el: Element): { x: number; y: number } {
	const root = document.documentElement;
	const rect = el.getBoundingClientRect();
	const w = Math.max(1, root.scrollWidth || window.innerWidth);
	const h = Math.max(1, root.scrollHeight || window.innerHeight);
	const x = clamp01((rect.left + rect.width / 2 + (window.scrollX || 0)) / w);
	const y = clamp01((rect.top + rect.height / 2 + (window.scrollY || 0)) / h);
	return { x, y };
}

async function capturePng(el: Element): Promise<{ base64: string; width: number; height: number }> {
	const dataUrl = await domToPng(el as HTMLElement, {
		scale: 1,
		backgroundColor: null,
		timeout: 5000,
	});
	const base64 = dataUrl.slice(dataUrl.indexOf(',') + 1);
	const img = new Image();
	img.src = dataUrl;
	await img.decode();
	return { base64, width: img.naturalWidth, height: img.naturalHeight };
}

function isExcludedRoot(el: Element): boolean {
	return el === document.documentElement || el === document.body;
}

/** Capture a `PickPayload` for an element and mouse location. */
async function makePickPayload(
	el: Element,
	clientX: number,
	clientY: number
): Promise<M.PickPayload> {
	const [shot, pos] = await Promise.all([capturePng(el), Promise.resolve(computePosition(el))]);
	return {
		selector: deriveSelector(el),
		positionX: pos.x,
		positionY: pos.y,
		screenshotBase64: shot.base64,
		screenshotWidth: shot.width,
		screenshotHeight: shot.height,
		elementLabel: labelFor(el),
		clientX,
		clientY,
	};
}

function postToHost(data: M.ChildToHostMessage): void {
	window.parent.postMessage(M.wrapChildMessage(data), '*');
}

function setupHostBridge(themeHandle: ThemeHandle): void {
	let pickHandler: ((e: MouseEvent) => void) | null = null;
	let hoverHandler: ((e: MouseEvent) => void) | null = null;
	let clickHandler: ((e: MouseEvent) => void) | null = null;
	let leaveHandler: (() => void) | null = null;
	let textClickHandler: ((e: MouseEvent) => void) | null = null;
	let ro: ResizeObserver | null = null;
	let mo: MutationObserver | null = null;
	let watchingSelectors: string[] = [];

	const editingRef: {
		current: { el: HTMLElement; selector: string; originalHtml: string } | null;
	} = { current: null };

	function resolvePinRects(selectors: string[]): M.PinResolution[] {
		return selectors.map((selector) => {
			let el: Element | null = null;
			try {
				el = document.querySelector(selector);
			} catch {
				el = null;
			}
			return {
				selector,
				found: !!el,
				rect: el ? serialiseRect(el.getBoundingClientRect()) : null,
			};
		});
	}

	function sendPinUpdate(): void {
		postToHost({ kind: 'pin-update', results: resolvePinRects(watchingSelectors) });
	}

	function stopHover(): void {
		if (hoverHandler) document.removeEventListener('mousemove', hoverHandler, true);
		if (clickHandler) document.removeEventListener('click', clickHandler, true);
		if (leaveHandler) document.removeEventListener('mouseleave', leaveHandler, true);
		hoverHandler = null;
		clickHandler = null;
		leaveHandler = null;
	}

	function stopPick(): void {
		if (pickHandler) document.removeEventListener('contextmenu', pickHandler, true);
		pickHandler = null;
	}

	function stopTextEdit(): void {
		const cur = editingRef.current;
		if (cur) {
			cur.el.innerHTML = cur.originalHtml;
			cur.el.contentEditable = 'inherit';
			editingRef.current = null;
		}
		if (textClickHandler) document.removeEventListener('click', textClickHandler, true);
		textClickHandler = null;
	}

	function stopPinWatch(): void {
		watchingSelectors = [];
		ro?.disconnect();
		mo?.disconnect();
		ro = null;
		mo = null;
	}

	window.addEventListener('message', (e) => {
		if (!M.isIkengaHostMessage(e.data)) return;
		// Only the host frame drives this bridge. Without this the verbs below
		// — start-text-edit, capture, start-pick — are reachable by anything
		// that can get a handle to this window and post to it. The host has
		// always checked its side (`e.source !== iframe.contentWindow`); this
		// end checked nothing, so the "origin check on both ends" the channel
		// contract calls for existed on one.
		if (!M.isFromExpectedSender(e, window.parent)) return;
		const m = (e.data as M.HostMessageWrapper).data;

		switch (m.kind) {
			case 'ping': {
				postToHost({ kind: 'pong' });
				return;
			}
			case 'theme': {
				themeHandle.applyFromHost(m.payload);
				return;
			}
			case 'start-pick': {
				stopPick();
				pickHandler = (ev: MouseEvent) => {
					const el = elementFromEventTarget(ev);
					if (!el || isExcludedRoot(el)) return;
					ev.preventDefault();
					ev.stopPropagation();
					void makePickPayload(el, ev.clientX, ev.clientY)
						.then((payload) => postToHost({ kind: 'pick', payload }))
						.catch((err) => console.error('[ikenga.bridge] pick capture failed', err));
				};
				document.addEventListener('contextmenu', pickHandler, true);
				return;
			}
			case 'stop-pick': {
				stopPick();
				return;
			}
			case 'start-comment': {
				stopHover();
				hoverHandler = (ev: MouseEvent) => {
					const el = elementFromEventTarget(ev);
					if (!el) {
						postToHost({ kind: 'hover', rect: null });
						return;
					}
					postToHost({ kind: 'hover', rect: serialiseRect(el.getBoundingClientRect()) });
				};
				clickHandler = (ev: MouseEvent) => {
					const el = elementFromEventTarget(ev);
					if (!el || isExcludedRoot(el)) return;
					ev.preventDefault();
					ev.stopPropagation();
					void makePickPayload(el, ev.clientX, ev.clientY)
						.then((payload) => postToHost({ kind: 'comment-pick', payload }))
						.catch((err) => console.error('[ikenga.bridge] comment capture failed', err));
				};
				leaveHandler = () => postToHost({ kind: 'hover', rect: null });
				document.addEventListener('mousemove', hoverHandler, true);
				document.addEventListener('click', clickHandler, true);
				document.addEventListener('mouseleave', leaveHandler, true);
				return;
			}
			case 'stop-comment': {
				stopHover();
				return;
			}
			case 'start-text-edit': {
				stopHover();
				hoverHandler = (ev: MouseEvent) => {
					const el = elementFromEventTarget(ev);
					postToHost({
						kind: 'hover',
						rect: el ? serialiseRect(el.getBoundingClientRect()) : null,
					});
				};
				textClickHandler = (ev: MouseEvent) => {
					const el = elementFromEventTarget(ev) as HTMLElement | null;
					if (!el || isExcludedRoot(el)) return;
					ev.preventDefault();
					ev.stopPropagation();
					if (editingRef.current) return;
					editingRef.current = {
						el,
						selector: deriveSelector(el),
						originalHtml: el.innerHTML,
					};
					el.contentEditable = 'true';
					el.focus();
					postToHost({
						kind: 'text-edit-pick',
						selector: editingRef.current.selector,
						rect: serialiseRect(el.getBoundingClientRect()),
						originalHtml: editingRef.current.originalHtml,
					});
				};
				leaveHandler = () => postToHost({ kind: 'hover', rect: null });
				document.addEventListener('mousemove', hoverHandler, true);
				document.addEventListener('click', textClickHandler, true);
				document.addEventListener('mouseleave', leaveHandler, true);
				return;
			}
			case 'stop-text-edit': {
				stopTextEdit();
				stopHover();
				return;
			}
			case 'resolve-pins': {
				postToHost({ kind: 'pins', requestId: m.requestId, results: resolvePinRects(m.selectors) });
				return;
			}
			case 'watch-pins': {
				stopPinWatch();
				watchingSelectors = m.selectors;
				ro = new ResizeObserver(() => sendPinUpdate());
				ro.observe(document.documentElement);
				mo = new MutationObserver(() => sendPinUpdate());
				mo.observe(document.documentElement, {
					attributes: true,
					childList: true,
					subtree: true,
					characterData: true,
				});
				document.addEventListener('scroll', sendPinUpdate, true);
				sendPinUpdate();
				return;
			}
			case 'unwatch-pins': {
				stopPinWatch();
				return;
			}
			case 'capture': {
				let el: Element | null = null;
				try {
					el = document.querySelector(m.selector);
				} catch {
					el = null;
				}
				if (!el) {
					postToHost({
						kind: 'capture-result',
						requestId: m.requestId,
						base64: '',
						width: 0,
						height: 0,
						error: `selector did not resolve: ${m.selector}`,
					});
					return;
				}
				void capturePng(el)
					.then((shot) =>
						postToHost({
							kind: 'capture-result',
							requestId: m.requestId,
							base64: shot.base64,
							width: shot.width,
							height: shot.height,
						})
					)
					.catch((err) =>
						postToHost({
							kind: 'capture-result',
							requestId: m.requestId,
							base64: '',
							width: 0,
							height: 0,
							// Include the stack: this runs inside an opaque-origin frame,
							// so the host cannot open devtools on the child and the message
							// alone rarely identifies the thrower.
							error: err instanceof Error ? `${err.message} | ${err.stack ?? ''}` : String(err),
						})
					);
				return;
			}
			default: {
				// eslint-disable-next-line @typescript-eslint/no-explicit-any
				const _exhaustive: never = m;
				void _exhaustive;
			}
		}
	});

	// Text-edit keyboard lifecycle is handled once at bridge install.
	document.addEventListener(
		'keydown',
		(e) => {
			const cur = editingRef.current;
			if (!cur) return;
			if (e.key === 'Escape') {
				e.preventDefault();
				cur.el.innerHTML = cur.originalHtml;
				cur.el.contentEditable = 'inherit';
				editingRef.current = null;
				postToHost({ kind: 'text-edit-cancel', selector: cur.selector });
				return;
			}
			if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
				e.preventDefault();
				const newHtml = cur.el.innerHTML;
				cur.el.contentEditable = 'inherit';
				editingRef.current = null;
				postToHost({
					kind: 'text-edit-commit',
					selector: cur.selector,
					innerHtml: newHtml,
					originalHtml: cur.originalHtml,
				});
			}
		},
		true
	);

	document.addEventListener(
		'blur',
		(e) => {
			const cur = editingRef.current;
			if (!cur || e.target !== cur.el) return;
			const newHtml = cur.el.innerHTML;
			cur.el.contentEditable = 'inherit';
			editingRef.current = null;
			postToHost({
				kind: 'text-edit-commit',
				selector: cur.selector,
				innerHtml: newHtml,
				originalHtml: cur.originalHtml,
			});
		},
		true
	);

	// Send ready so the host can forward its theme and any armed modes.
	postToHost({ kind: 'ready' });
}

// ── Mount ────────────────────────────────────────────────────────────────

export function mountArtifactBridge(): void {
	// Idempotent: if an inline polyfill or a previous injection already
	// populated the bridge, do nothing. The host descriptor may still be
	// missing in that case (an inline polyfill won't set it), so set it
	// defensively without clobbering an existing value.
	if (!window.__ikenga_host__) {
		window.__ikenga_host__ = { kind: 'ikenga', user: null };
	}
	if (window.__ikenga_bridge_polyfill__) return;

	// Mirror the host theme onto the artifact ASAP — before the manifest-parse
	// early-returns below, so even a manifest-less HTML preview tracks the
	// shell's dark/light + palette. The handle is re-exposed on `art.theme`.
	const themeHandle = setupTheme();

	// Start the postMessage host bridge so the sandboxed child can receive
	// theme updates, picker/comment/text-edit commands, and pin requests.
	setupHostBridge(themeHandle);

	// Parse manifest. If absent or malformed, leave the host descriptor in
	// place but skip installing the polyfill — any inline polyfill in the
	// page will take over, and an authoring error is more useful than a
	// confusing partial bridge.
	const parsed = parseTagJson<Manifest>('ikenga-manifest');
	if (!parsed || typeof parsed.id !== 'string') {
		return;
	}
	// Aliased post-narrow so TS keeps the non-null type inside closures.
	const manifest: Manifest = parsed;

	const mock = parseTagJson<Record<string, unknown>>('ikenga-mock-data') ?? {};
	const dataSources = manifest.dataSources ?? {};

	const cache: Record<string, unknown> = {};
	const sourceSubs: Record<string, Array<(v: unknown) => void>> = {};
	const usedFallback: Record<string, boolean> = {};
	const intervalHandles: Record<string, ReturnType<typeof setInterval>> = {};

	const stateSubs: Record<string, Array<(v: unknown) => void>> = {};
	const stateNs = `ikenga:${manifest.id}:`;

	function resolve(name: string): Promise<unknown> {
		const def = dataSources[name];
		if (!def) {
			usedFallback[name] = true;
			return Promise.resolve(mock[name] ?? null);
		}

		if (def.type === 'fetch') {
			const fs = def as FetchSource;
			return fetch(fs.url, {
				method: fs.method || 'GET',
				headers: fs.headers,
			})
				.then((res) => {
					if (!res.ok) throw new Error(`http ${res.status}`);
					return res.json();
				})
				.then((data) => {
					usedFallback[name] = false;
					return data;
				})
				.catch(() => {
					usedFallback[name] = true;
					return name in mock ? mock[name] : null;
				});
		}

		if (def.type === 'file') {
			const fsrc = def as FileSource;
			const url = resolveArtifactRelative(fsrc.path);
			if (!url) {
				// Escapes the mount, or is absolute/remote — refuse rather than
				// let an artifact read outside the directory it was served from.
				console.warn('[ikenga.source] rejected file path', fsrc.path);
				usedFallback[name] = true;
				return Promise.resolve(name in mock ? mock[name] : null);
			}
			// Same-origin GET against the viewer-server mount (ServeDir over the
			// artifact's own directory), so no Tauri/Node import is needed and
			// the iframe-only constraint holds. `cache: 'no-store'` because these
			// files are rewritten out-of-band by cron.
			return fetch(url, { cache: 'no-store' })
				.then((res) => {
					if (!res.ok) throw new Error(`http ${res.status}`);
					return res.json();
				})
				.then((data) => {
					usedFallback[name] = false;
					return data;
				})
				.catch(() => {
					usedFallback[name] = true;
					return name in mock ? mock[name] : null;
				});
		}

		// supabase | sql | mcp → mock-only in v0.
		usedFallback[name] = true;
		return Promise.resolve(name in mock ? mock[name] : null);
	}

	function fireSourceSubs(name: string, value: unknown): void {
		const subs = sourceSubs[name];
		if (!subs) return;
		// Iterate a copy so unsubscribes during dispatch don't skip entries.
		for (const fn of subs.slice()) {
			try {
				fn(value);
			} catch (err) {
				console.error('[ikenga.source] subscriber threw', err);
			}
		}
	}

	function refreshSource(name: string): Promise<void> {
		return resolve(name).then((v) => {
			cache[name] = v;
			fireSourceSubs(name, v);
		});
	}

	function setupRefreshMode(name: string, def: DataSource): void {
		const mode = def.refresh?.mode ?? 'manual';
		if (mode === 'interval') {
			const ms = parseDuration(def.refresh?.every);
			if (ms !== null && ms > 0) {
				intervalHandles[name] = setInterval(() => {
					void refreshSource(name);
				}, ms);
			}
		}
		// 'manual' → nothing to wire. 'watch' → no-op in v0 (Phase 2 routes
		// to host fs_watch).
	}

	function makeSourceHandle(name: string): SourceHandle {
		return {
			get: () => cache[name],
			subscribe: (fn) => {
				if (!sourceSubs[name]) sourceSubs[name] = [];
				sourceSubs[name].push(fn);
				return () => {
					const arr = sourceSubs[name];
					if (!arr) return;
					sourceSubs[name] = arr.filter((f) => f !== fn);
				};
			},
			refresh: () => refreshSource(name),
		};
	}

	const stateHandle: StateHandle = {
		get: (key) => {
			try {
				const raw = localStorage.getItem(stateNs + key);
				return raw === null ? null : JSON.parse(raw);
			} catch {
				return null;
			}
		},
		set: (key, value) => {
			try {
				localStorage.setItem(stateNs + key, JSON.stringify(value));
			} catch (err) {
				console.warn('[ikenga.state] failed to persist', key, err);
			}
			const subs = stateSubs[key];
			if (!subs) return;
			for (const fn of subs.slice()) {
				try {
					fn(value);
				} catch (err) {
					console.error('[ikenga.state] subscriber threw', err);
				}
			}
		},
		subscribe: (key, fn) => {
			if (!stateSubs[key]) stateSubs[key] = [];
			stateSubs[key].push(fn);
			return () => {
				const arr = stateSubs[key];
				if (!arr) return;
				stateSubs[key] = arr.filter((f) => f !== fn);
			};
		},
	};

	const notesHandle: NotesHandle = {
		send: (text, opts) => {
			// v0: log a structured payload. Phase 2 routes via postMessage
			// back to the originating terminal session.
			console.log('[ikenga.notes]', {
				artifactId: manifest.id,
				text,
				opts: opts ?? {},
			});
		},
	};

	function init(): Promise<Art> {
		const keys = Object.keys(dataSources);
		return Promise.all(
			keys.map((k) =>
				resolve(k).then((v) => {
					cache[k] = v;
				})
			)
		).then(() => {
			// Wire refresh modes after the initial fetch so interval timers
			// don't double-fire during init.
			for (const k of keys) {
				const def = dataSources[k];
				if (def) setupRefreshMode(k, def);
			}

			const host = window.__ikenga_host__ ?? { kind: 'ikenga', user: null };

			return {
				manifest,
				host: {
					kind: host.kind,
					user: host.user,
					usedFallback: (n: string) => !!usedFallback[n],
					anyFallback: () => Object.values(usedFallback).some(Boolean),
				},
				source: makeSourceHandle,
				state: stateHandle,
				notes: notesHandle,
				theme: themeHandle,
				pin: () => {
					// v0 stub — Phase 2 will postMessage a pin-request to the
					// shell viewer host, which adds the artifact to the
					// activity bar.
					console.log('[ikenga.pin] requested', { artifactId: manifest.id });
				},
			};
		});
	}

	window.__ikenga_bridge_polyfill__ = { init };
}
