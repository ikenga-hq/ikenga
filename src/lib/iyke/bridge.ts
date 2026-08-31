// Iyke FE bridge. Mounted once inside <Workspace />. Handles:
//
//   1. Module-level instrumentation: console.* and fetch/XHR shims push
//      log + network entries to the Rust ring buffers via Tauri commands.
//   2. Event listeners for `iyke://*` requests from the Rust HTTP server:
//      DOM snapshot, query-cache dump, wait-predicate, click/type/key.
//
// HMR safety: instrumentation guards re-installation with a __iyke_patched
// symbol on the global. The mount hook uses a single-flight ref so React
// strict-mode double-invokes don't mount listeners twice.
//
// Only mount in a top-level shell context (matches useIykeShellSync /
// useIykeControlListener). Never mount inside per-pane MemoryRouter
// contexts.

import { useEffect } from 'react';
import { invoke, listen, type UnlistenFn } from '@/lib/tauri-cmd';
import { findLeaf, getLeafIdsInOrder } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { PaneNode } from '@/lib/panes/types';
import { queryClient } from '@/lib/query-client';
import { readCapture, stripAnsi } from '@/terminal/pty-output-buffer';
import { getPty } from '@/terminal/pty-registry';
import { createTerminalSession } from '@/terminal/single-terminal';
import { resolvePaneScope, useIykeActivity } from './activity-store';
import {
	allSearchDocs,
	type DomSnapshotResult,
	resolveBySelector,
	resolveByText,
	resolveRef,
	sameOriginIframeBody,
	takeSnapshot,
} from './dom-snapshot';
import {
	currentStateGeneration,
	getIframe,
	type IframeRegistration,
	installIykeIframeMessageListener,
	postToIframeFireAndForget,
	requestIframeDom,
	requestIframeWait,
} from './iframe-registry';

// ── Types ────────────────────────────────────────────────────────────────

interface LogEntry {
	ts: number;
	level: 'log' | 'info' | 'warn' | 'error' | 'debug';
	message: string;
	source?: string;
	stack?: string;
}

interface NetworkEntry {
	ts: number;
	method: string;
	url: string;
	status?: number;
	duration_ms: number;
	kind: 'fetch' | 'xhr';
	error?: string;
	source?: string;
}

interface DomRequestPayload {
	request_id: string;
	pane?: string | null;
	query?: string | null;
	all?: boolean;
}

interface QueryCacheRequestPayload {
	request_id: string;
	pane?: string | null;
}

interface WaitRequestPayload {
	request_id: string;
	kind: 'text' | 'selector' | 'ref' | 'gone-text' | 'gone-selector';
	value: string;
	timeout_ms: number;
	pane?: string | null;
}

interface ClickPayload {
	request_id: string;
	ref?: string | null;
	selector?: string | null;
	text?: string | null;
	pane?: string | null;
}

interface TypePayload {
	ref?: string | null;
	selector?: string | null;
	text: string;
	replace?: boolean;
	pane?: string | null;
}

interface KeyPayload {
	combo: string;
	ref?: string | null;
	selector?: string | null;
	pane?: string | null;
}

interface TerminalSendPayload {
	request_id?: string;
	pane?: string | null;
	data?: string | null;
	keys?: string[] | null;
}

interface TerminalReadPayload {
	request_id: string;
	pane?: string | null;
	bytes?: number | null;
	raw?: boolean | null;
}

interface TerminalReadResult {
	text: string;
	bytes_available: number;
	bytes_returned: number;
	session_id: string | null;
	terminal_id: string | null;
	pty_id: string | null;
	start_offset: number;
	end_offset: number;
	available_start_offset: number;
	truncated: boolean;
	exited: boolean;
	exit_code: number | null;
	error: string | null;
}

// ── Instrumentation (module side-effects) ────────────────────────────────

const PATCH_FLAG = Symbol.for('@royalti/iyke-bridge/patched');
type PatchedGlobal = typeof globalThis & { [PATCH_FLAG]?: boolean };

const logBuffer: LogEntry[] = [];
const networkBuffer: NetworkEntry[] = [];
let logFlushTimer: number | null = null;
let netFlushTimer: number | null = null;

function flushLogsSoon() {
	if (logFlushTimer !== null) return;
	logFlushTimer = window.setTimeout(() => {
		logFlushTimer = null;
		if (logBuffer.length === 0) return;
		const batch = logBuffer.splice(0, logBuffer.length);
		invoke('iyke_log_push', { entries: batch }).catch(() => {
			// Tauri not ready yet, requeue silently.
			logBuffer.unshift(...batch);
		});
	}, 250);
}

function flushNetworkSoon() {
	if (netFlushTimer !== null) return;
	netFlushTimer = window.setTimeout(() => {
		netFlushTimer = null;
		if (networkBuffer.length === 0) return;
		const batch = networkBuffer.splice(0, networkBuffer.length);
		invoke('iyke_network_push', { entries: batch }).catch(() => {
			networkBuffer.unshift(...batch);
		});
	}, 250);
}

function pushLog(entry: LogEntry) {
	logBuffer.push(entry);
	if (logBuffer.length > 1000) logBuffer.splice(0, logBuffer.length - 500);
	flushLogsSoon();
}

function pushNetwork(entry: NetworkEntry) {
	networkBuffer.push(entry);
	if (networkBuffer.length > 200) networkBuffer.splice(0, networkBuffer.length - 100);
	flushNetworkSoon();
}

function stringifyArgs(args: unknown[]): string {
	return args
		.map((a) => {
			if (a instanceof Error) return `${a.name}: ${a.message}`;
			if (typeof a === 'string') return a;
			try {
				return JSON.stringify(a);
			} catch {
				return String(a);
			}
		})
		.join(' ');
}

function patchConsole() {
	const levels: LogEntry['level'][] = ['log', 'info', 'warn', 'error', 'debug'];
	for (const level of levels) {
		const original = console[level].bind(console);
		console[level] = (...args: unknown[]) => {
			try {
				pushLog({
					ts: Date.now(),
					level,
					message: stringifyArgs(args),
					source: 'shell',
					stack: level === 'error' ? (new Error().stack ?? undefined) : undefined,
				});
			} catch {
				/* never let shim breakage break logging */
			}
			original(...args);
		};
	}
	// Capture unhandled errors + rejections too.
	window.addEventListener('error', (e) => {
		pushLog({
			ts: Date.now(),
			level: 'error',
			message: e.message || 'window error',
			source: 'shell',
			stack: e.error?.stack,
		});
	});
	window.addEventListener('unhandledrejection', (e) => {
		const reason = e.reason;
		pushLog({
			ts: Date.now(),
			level: 'error',
			message:
				reason instanceof Error
					? `unhandled rejection: ${reason.message}`
					: `unhandled rejection: ${String(reason)}`,
			source: 'shell',
			stack: reason instanceof Error ? reason.stack : undefined,
		});
	});
}

function isIykeIpc(url: string): boolean {
	// Tauri's invoke() routes through fetch under the hood. Capturing those
	// would create a feedback loop (network shim pushes via invoke → fetch
	// captures → push → ...). Drop ipc:// + tauri:// + the iyke control
	// bridge endpoint itself.
	return url.startsWith('ipc://') || url.startsWith('tauri://') || url.includes('/iyke/');
}

function patchFetch() {
	const original = window.fetch.bind(window);
	window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
		const start = performance.now();
		const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
		const method = (
			init?.method ?? (input instanceof Request ? input.method : 'GET')
		).toUpperCase();
		if (isIykeIpc(url)) {
			return original(input as RequestInfo, init);
		}
		try {
			const res = await original(input as RequestInfo, init);
			pushNetwork({
				ts: Date.now(),
				method,
				url,
				status: res.status,
				duration_ms: Math.round(performance.now() - start),
				kind: 'fetch',
				source: 'shell',
			});
			return res;
		} catch (err) {
			pushNetwork({
				ts: Date.now(),
				method,
				url,
				duration_ms: Math.round(performance.now() - start),
				kind: 'fetch',
				error: err instanceof Error ? err.message : String(err),
				source: 'shell',
			});
			throw err;
		}
	};
}

function patchXhr() {
	const OrigOpen = XMLHttpRequest.prototype.open;
	const OrigSend = XMLHttpRequest.prototype.send;
	XMLHttpRequest.prototype.open = function (
		this: XMLHttpRequest,
		method: string,
		url: string | URL,
		...rest: unknown[]
	) {
		(this as XMLHttpRequest & { __iyke_method?: string; __iyke_url?: string }).__iyke_method =
			method.toUpperCase();
		(this as XMLHttpRequest & { __iyke_method?: string; __iyke_url?: string }).__iyke_url =
			typeof url === 'string' ? url : url.href;
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		return OrigOpen.apply(this, [method, url, ...(rest as any[])] as any);
	} as typeof XMLHttpRequest.prototype.open;
	XMLHttpRequest.prototype.send = function (
		this: XMLHttpRequest,
		body?: Document | XMLHttpRequestBodyInit | null
	) {
		const xhr = this as XMLHttpRequest & { __iyke_method?: string; __iyke_url?: string };
		if (xhr.__iyke_url && isIykeIpc(xhr.__iyke_url)) {
			return OrigSend.call(this, body ?? null);
		}
		const start = performance.now();
		const cleanup = () => {
			pushNetwork({
				ts: Date.now(),
				method: xhr.__iyke_method ?? 'GET',
				url: xhr.__iyke_url ?? '',
				status: xhr.status || undefined,
				duration_ms: Math.round(performance.now() - start),
				kind: 'xhr',
				error: xhr.status === 0 && xhr.readyState === 4 ? 'network error' : undefined,
				source: 'shell',
			});
		};
		xhr.addEventListener('loadend', cleanup);
		return OrigSend.call(this, body ?? null);
	};
}

export function installInstrumentation() {
	const g = globalThis as PatchedGlobal;
	if (g[PATCH_FLAG]) return;
	g[PATCH_FLAG] = true;
	patchConsole();
	patchFetch();
	patchXhr();
}

// Run immediately on import — module-level side effect, idempotent via flag.
installInstrumentation();

// ── Tauri event listeners ────────────────────────────────────────────────

// Resolve a `pane` request param to an iframe registration. Three forms work:
//   1. a registry key (full or unique prefix) — html-frame panes register by
//      pane id; pkg iframes register by pkg id (`com.ikenga.tasks`).
//   2. a pane-leaf id (full or unique prefix, matching the truncated ids
//      `iyke state` prints) whose active tab is a `/pkg/<pkgId>/…` route —
//      resolved through the pane tree to the pkg's registration.
//   3. `shell` / empty → undefined (host targeting).
function resolveIframeReg(pane: string | null | undefined): IframeRegistration | undefined {
	if (!pane || pane === 'shell') return undefined;
	const direct = getIframe(pane);
	if (direct) return direct;
	const state = usePaneStore.getState();
	const ids = getLeafIdsInOrder(state.root);
	const leafId = ids.includes(pane) ? pane : uniquePrefixMatch(ids, pane);
	if (!leafId) return undefined;
	const leaf = findLeaf(state.root, leafId);
	if (!leaf || leaf.type !== 'leaf') return undefined;
	const tab = leaf.tabs[leaf.activeTabIdx];
	if (!tab || tab.kind !== 'route') return undefined;
	const m = /^\/pkg\/([^/]+)/.exec(tab.path);
	return m ? getIframe(m[1]) : undefined;
}

function uniquePrefixMatch(candidates: string[], prefix: string): string | undefined {
	const matches = candidates.filter((c) => c.startsWith(prefix));
	return matches.length === 1 ? matches[0] : undefined;
}

// For a registration the iframe-side bridge never said `hello` on (pkg
// iframes mount the AppBridge, not the iyke iframe bridge), serve requests
// host-side against its same-origin document instead of posting into the
// iframe — the postMessage path would just time out. Returns null when the
// bridged path should be used (or the iframe is cross-origin).
function directDocFor(reg: IframeRegistration): Document | null {
	if (reg.bridged) return null;
	return sameOriginIframeBody(reg.iframe)?.ownerDocument ?? null;
}

async function handleDomRequest(payload: DomRequestPayload) {
	const scope = payload.pane && payload.pane !== 'shell' ? payload.pane : 'window';
	const actId = useIykeActivity.getState().begin({
		kind: 'dom',
		scope,
		detail: payload.query ?? undefined,
	});
	try {
		let result: { text: string; json: unknown; generation: number };
		const reg = resolveIframeReg(payload.pane);
		const directDoc = reg ? directDocFor(reg) : null;
		if (reg && !directDoc) {
			result = await requestIframeDom(reg.paneId, payload.query ?? undefined, payload.all === true);
		} else {
			const snap: DomSnapshotResult = takeSnapshot({
				query: payload.query ?? undefined,
				all: payload.all === true,
				root: directDoc?.body ?? undefined,
			});
			result = { text: snap.text, json: snap.json, generation: snap.generation };
		}
		await invoke('iyke_dom_done', {
			requestId: payload.request_id,
			result,
		});
	} catch (err) {
		await invoke('iyke_dom_done', {
			requestId: payload.request_id,
			result: {
				text: `error: ${err instanceof Error ? err.message : String(err)}`,
				json: [],
				generation: 0,
			},
		}).catch((doneErr) => {
			console.error('[iyke] iyke_dom_done error-reply failed:', doneErr);
		});
	} finally {
		useIykeActivity.getState().end(actId);
	}
}

interface QueryEntry {
	queryKey: unknown;
	status: string;
	fetchStatus: string;
	isStale: boolean;
	dataUpdatedAt: number;
	errorUpdatedAt: number;
	error?: string;
	dataPreview?: string;
}

async function handleQueryCacheRequest(payload: QueryCacheRequestPayload) {
	const scope = payload.pane && payload.pane !== 'shell' ? payload.pane : 'window';
	const actId = useIykeActivity.getState().begin({ kind: 'query-cache', scope });
	try {
		const cache = queryClient.getQueryCache();
		const entries: QueryEntry[] = cache.getAll().map((q) => {
			const state = q.state;
			let dataPreview: string | undefined;
			if (state.data !== undefined && state.data !== null) {
				try {
					const s = JSON.stringify(state.data);
					dataPreview = s.length > 200 ? s.slice(0, 200) + '…' : s;
				} catch {
					dataPreview = String(state.data);
				}
			}
			return {
				queryKey: q.queryKey,
				status: state.status,
				fetchStatus: state.fetchStatus,
				isStale: q.isStale(),
				dataUpdatedAt: state.dataUpdatedAt,
				errorUpdatedAt: state.errorUpdatedAt,
				error: state.error instanceof Error ? state.error.message : undefined,
				dataPreview,
			};
		});
		await invoke('iyke_query_cache_done', {
			requestId: payload.request_id,
			result: { entries },
		});
	} catch (err) {
		await invoke('iyke_query_cache_done', {
			requestId: payload.request_id,
			result: { entries: [{ error: err instanceof Error ? err.message : String(err) }] },
		}).catch((doneErr) => {
			console.error('[iyke] iyke_query_cache_done error-reply failed:', doneErr);
		});
	} finally {
		useIykeActivity.getState().end(actId);
	}
}

async function handleWaitRequest(payload: WaitRequestPayload) {
	const scope = payload.pane && payload.pane !== 'shell' ? payload.pane : 'window';
	const actId = useIykeActivity.getState().begin({
		kind: 'wait',
		scope,
		detail: `${payload.kind}:${payload.value}`,
	});
	const finish = () => useIykeActivity.getState().end(actId);
	const reg = resolveIframeReg(payload.pane);
	const directDoc = reg ? directDocFor(reg) : null;
	if (reg && !directDoc) {
		try {
			const r = await requestIframeWait(
				reg.paneId,
				payload.kind,
				payload.value,
				payload.timeout_ms
			);
			await invoke('iyke_wait_done', {
				requestId: payload.request_id,
				result: r,
			});
		} catch (err) {
			await invoke('iyke_wait_done', {
				requestId: payload.request_id,
				result: {
					satisfied: false,
					elapsed_ms: 0,
					message: err instanceof Error ? err.message : String(err),
				},
			}).catch((doneErr) => {
				console.error('[iyke] iyke_wait_done error-reply failed:', doneErr);
			});
		} finally {
			finish();
		}
		return;
	}
	const start = performance.now();
	const deadline = start + payload.timeout_ms;
	// Text predicates span the host doc + same-origin iframe docs (pkg panes)
	// — or just the pane's doc when the request targeted one directly.
	const visibleText = (): string =>
		(directDoc ? [directDoc] : allSearchDocs()).map((d) => d.body?.innerText ?? '').join('\n');
	const check = (): boolean => {
		switch (payload.kind) {
			case 'text': {
				return visibleText().includes(payload.value);
			}
			case 'gone-text': {
				return !visibleText().includes(payload.value);
			}
			case 'selector': {
				const el = resolveBySelector(payload.value, directDoc ?? undefined);
				// `'offsetParent' in el` instead of instanceof HTMLElement —
				// cross-realm safe for elements inside iframe documents.
				return Boolean(el && 'offsetParent' in el && (el as HTMLElement).offsetParent !== null);
			}
			case 'gone-selector': {
				const el = resolveBySelector(payload.value, directDoc ?? undefined);
				return !el || ('offsetParent' in el && (el as HTMLElement).offsetParent === null);
			}
			case 'ref': {
				const el = resolveRef(payload.value);
				return Boolean(el);
			}
		}
	};

	return new Promise<void>((resolve) => {
		const tick = async () => {
			const now = performance.now();
			if (check()) {
				await invoke('iyke_wait_done', {
					requestId: payload.request_id,
					result: { satisfied: true, elapsed_ms: Math.round(now - start) },
				}).catch(() => {});
				finish();
				resolve();
				return;
			}
			if (now >= deadline) {
				await invoke('iyke_wait_done', {
					requestId: payload.request_id,
					result: {
						satisfied: false,
						elapsed_ms: Math.round(now - start),
						message: `timeout after ${payload.timeout_ms}ms (kind=${payload.kind})`,
					},
				}).catch(() => {});
				finish();
				resolve();
				return;
			}
			setTimeout(tick, 50);
		};
		tick().catch(() => {
			finish();
			resolve();
		});
	});
}

function resolveTarget(
	ref: string | null | undefined,
	selector: string | null | undefined,
	text: string | null | undefined,
	doc?: Document
): Element | null {
	if (ref) return resolveRef(ref);
	if (selector) return resolveBySelector(selector, doc);
	if (text) return resolveByText(text, doc);
	return null;
}

async function handleClick(payload: ClickPayload) {
	let matched = false;
	try {
		const reg = resolveIframeReg(payload.pane);
		const directDoc = reg ? directDocFor(reg) : null;
		if (reg && !directDoc) {
			const id = useIykeActivity.getState().begin({
				kind: 'click',
				scope: reg.paneId,
				detail: payload.ref ?? payload.selector ?? payload.text ?? undefined,
			});
			postToIframeFireAndForget(reg.paneId, 'click', {
				ref: payload.ref ?? null,
				selector: payload.selector ?? null,
				text: payload.text ?? null,
			});
			useIykeActivity.getState().end(id);
			// Iframe clicks are fire-and-forget into the sidecar bridge, which
			// resolves the target on its side; the shell can't observe the
			// hit/miss without a round-trip through the iframe, so report
			// optimistically. TODO: thread the iframe doClick result back the
			// way requestIframeDom does, then report the real match here.
			matched = true;
			return;
		}
		// Host path — also covers same-origin pkg iframes (directDoc scopes the
		// selector/text resolution to the pane's document; unscoped resolution
		// searches the host doc + every same-origin iframe doc).
		const el = resolveTarget(payload.ref, payload.selector, payload.text, directDoc ?? undefined);
		if (!el) {
			console.warn('[iyke] click target not found:', payload);
			return;
		}
		matched = true;
		const id = useIykeActivity.getState().begin({
			kind: 'click',
			scope: resolvePaneScope(el),
			detail: payload.ref ?? payload.selector ?? payload.text ?? undefined,
		});
		if (typeof (el as HTMLElement).click === 'function') {
			// Duck-typed instead of `instanceof HTMLElement` — elements inside
			// iframe documents belong to another realm, so instanceof fails.
			// Match Playwright's hierarchy: focus first (so :focus styles + form
			// semantics fire), then dispatch a real click. .click() routes through
			// the user-activation path, which is what most React handlers expect.
			try {
				(el as HTMLElement).focus({ preventScroll: false });
			} catch {
				/* svg etc. */
			}
			(el as HTMLElement).click();
		} else {
			el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
		}
		useIykeActivity.getState().end(id);
	} finally {
		// Resolve the host's pending round-trip so `/iyke/click` returns the
		// real outcome — `ok:true` on a hit, `ok:false` ("target not found")
		// on a miss — instead of the old blind `ok:true`.
		await invoke('iyke_action_done', {
			requestId: payload.request_id,
			result: { matched },
		}).catch(() => {});
	}
}

function handleType(payload: TypePayload) {
	const reg = resolveIframeReg(payload.pane);
	const directDoc = reg ? directDocFor(reg) : null;
	if (reg && !directDoc) {
		const id = useIykeActivity.getState().begin({
			kind: 'type',
			scope: reg.paneId,
			detail: payload.ref ?? payload.selector ?? undefined,
		});
		postToIframeFireAndForget(reg.paneId, 'type', {
			ref: payload.ref ?? null,
			selector: payload.selector ?? null,
			text: payload.text,
			replace: payload.replace === true,
		});
		useIykeActivity.getState().end(id);
		return;
	}
	const el = resolveTarget(payload.ref, payload.selector, null, directDoc ?? undefined);
	if (!el) {
		console.warn('[iyke] type target not found:', payload);
		return;
	}
	const id = useIykeActivity.getState().begin({
		kind: 'type',
		scope: resolvePaneScope(el),
		detail: payload.ref ?? payload.selector ?? undefined,
	});
	// Tag-name checks instead of instanceof — elements inside same-origin
	// iframe documents belong to another realm, where instanceof fails.
	const tag = el.tagName;
	const isField = tag === 'INPUT' || tag === 'TEXTAREA';
	if (isField || (el as HTMLElement).isContentEditable) {
		(el as HTMLElement).focus();
		if (isField) {
			const field = el as HTMLInputElement | HTMLTextAreaElement;
			// React's controlled inputs need the *native* value setter from the
			// element's own realm, then a bubbling input event.
			const win = (el.ownerDocument.defaultView ?? window) as typeof window;
			const proto =
				tag === 'INPUT' ? win.HTMLInputElement.prototype : win.HTMLTextAreaElement.prototype;
			const setter = Object.getOwnPropertyDescriptor(proto, 'value')?.set;
			const next = payload.replace ? payload.text : `${field.value}${payload.text}`;
			if (setter) setter.call(field, next);
			else field.value = next;
			el.dispatchEvent(new Event('input', { bubbles: true }));
			el.dispatchEvent(new Event('change', { bubbles: true }));
		} else {
			// contenteditable
			if (payload.replace) (el as HTMLElement).innerText = payload.text;
			else (el as HTMLElement).innerText += payload.text;
			el.dispatchEvent(new Event('input', { bubbles: true }));
		}
	} else {
		console.warn('[iyke] type: target is not editable', el.tagName);
	}
	useIykeActivity.getState().end(id);
}

const KEY_ALIASES: Record<string, string> = {
	enter: 'Enter',
	esc: 'Escape',
	escape: 'Escape',
	tab: 'Tab',
	space: ' ',
	up: 'ArrowUp',
	down: 'ArrowDown',
	left: 'ArrowLeft',
	right: 'ArrowRight',
	backspace: 'Backspace',
	delete: 'Delete',
	home: 'Home',
	end: 'End',
};

function parseCombo(combo: string): {
	key: string;
	ctrl: boolean;
	alt: boolean;
	shift: boolean;
	meta: boolean;
} {
	const parts = combo
		.split(/[+,]/)
		.map((p) => p.trim())
		.filter(Boolean);
	let key = '';
	let ctrl = false,
		alt = false,
		shift = false,
		meta = false;
	for (const p of parts) {
		const low = p.toLowerCase();
		if (low === 'ctrl' || low === 'control') ctrl = true;
		else if (low === 'alt' || low === 'option') alt = true;
		else if (low === 'shift') shift = true;
		else if (low === 'meta' || low === 'cmd' || low === 'command' || low === 'super') meta = true;
		else key = KEY_ALIASES[low] ?? p;
	}
	return { key, ctrl, alt, shift, meta };
}

// Map a key combo (e.g. "Enter", "Ctrl+C", "Up") to the terminal escape
// bytes xterm.js expects. Returns null if the combo isn't recognised.
function comboToTerminalBytes(combo: string): string | null {
	const parsed = parseCombo(combo);
	const key = parsed.key;
	// Ctrl+<letter|@|[|\|]|^|_> → C0 control byte (Ctrl+C=\x03, Ctrl+D=\x04, …).
	if (parsed.ctrl && !parsed.alt && !parsed.meta && key.length === 1) {
		const code = key.toUpperCase().charCodeAt(0);
		if (code >= 64 && code <= 95) return String.fromCharCode(code - 64);
		if (code >= 97 && code <= 122) return String.fromCharCode(code - 96);
	}
	const ESC = '\x1b';
	const altPrefix = parsed.alt ? ESC : '';
	switch (key) {
		case 'Enter':
			return altPrefix + '\r';
		case 'Tab':
			return altPrefix + '\t';
		case 'Escape':
			return altPrefix + ESC;
		case 'Backspace':
			return altPrefix + '\x7f';
		case ' ':
			return altPrefix + ' ';
		case 'ArrowUp':
			return ESC + '[A';
		case 'ArrowDown':
			return ESC + '[B';
		case 'ArrowRight':
			return ESC + '[C';
		case 'ArrowLeft':
			return ESC + '[D';
		case 'Home':
			return ESC + '[H';
		case 'End':
			return ESC + '[F';
		case 'Delete':
			return ESC + '[3~';
		case 'PageUp':
			return ESC + '[5~';
		case 'PageDown':
			return ESC + '[6~';
	}
	const fnMatch = /^F([1-9]|1[0-2])$/i.exec(key);
	if (fnMatch) {
		const n = Number(fnMatch[1]);
		// xterm: F1-F4 → SS3 (\x1bO[PQRS]); F5-F12 → CSI [n~]
		const ss3 = ['P', 'Q', 'R', 'S'];
		if (n <= 4) return ESC + 'O' + ss3[n - 1];
		const csi = [15, 17, 18, 19, 20, 21, 23, 24]; // F5..F12
		return ESC + '[' + csi[n - 5] + '~';
	}
	if (key.length === 1 && !parsed.ctrl && !parsed.meta) {
		// Plain printable, optionally Alt-prefixed.
		return altPrefix + key;
	}
	return null;
}

function resolveTerminalSessionId(paneId: string | null | undefined): string | null {
	const state = usePaneStore.getState();
	const leafId = paneId && paneId !== 'shell' ? paneId : state.focusedId;
	const leaf = findLeaf(state.root, leafId);
	if (!leaf || leaf.type !== 'leaf') return null;
	const active = leaf.tabs[leaf.activeTabIdx];
	if (!active || active.kind !== 'terminal') return null;
	return active.sessionId;
}

async function handleTerminalReadRequest(payload: TerminalReadPayload) {
	const actId = useIykeActivity.getState().begin({
		kind: 'dom',
		scope: payload.pane ?? 'focused',
		detail: 'terminal-read',
	});
	const replyOk = (result: TerminalReadResult) =>
		invoke('iyke_terminal_read_done', {
			requestId: payload.request_id,
			result,
		}).catch((err) => {
			console.error('[iyke] iyke_terminal_read_done failed:', err);
		});
	try {
		const sessionId = resolveTerminalSessionId(payload.pane ?? null);
		if (!sessionId) {
			await replyOk({
				text: '',
				bytes_available: 0,
				bytes_returned: 0,
				session_id: null,
				terminal_id: null,
				pty_id: null,
				start_offset: 0,
				end_offset: 0,
				available_start_offset: 0,
				truncated: false,
				exited: false,
				exit_code: null,
				error: 'pane has no active terminal tab',
			});
			return;
		}
		const buf = readCapture(sessionId);
		if (buf === null) {
			await replyOk({
				text: '',
				bytes_available: 0,
				bytes_returned: 0,
				session_id: sessionId,
				terminal_id: sessionId,
				pty_id: null,
				start_offset: 0,
				end_offset: 0,
				available_start_offset: 0,
				truncated: false,
				exited: false,
				exit_code: null,
				error: 'no capture buffer for session (pty not registered or detached)',
			});
			return;
		}
		const tailBytes = payload.bytes && payload.bytes > 0 ? payload.bytes : buf.length;
		const slice = tailBytes >= buf.length ? buf : buf.subarray(buf.length - tailBytes);
		const decoded = new TextDecoder('utf-8', { fatal: false }).decode(slice);
		const text = payload.raw ? decoded : stripAnsi(decoded);
		await replyOk({
			text,
			bytes_available: buf.length,
			bytes_returned: slice.length,
			session_id: sessionId,
			terminal_id: sessionId,
			pty_id: getPty(sessionId)?.id ?? null,
			start_offset: 0,
			end_offset: buf.length,
			available_start_offset: 0,
			truncated: false,
			exited: false,
			exit_code: null,
			error: null,
		});
	} finally {
		useIykeActivity.getState().end(actId);
	}
}

/** WP-08 — bridge-driven terminal creation.
 *
 * Terminal creation lives here rather than in Rust because the frontend owns
 * the session store and the pane tree. A Rust-local PTY would be invisible in
 * the UI: unwatchable, un-poppable, unreclaimable. Going through the frontend
 * means an agent's terminal is an ordinary tab.
 *
 * The new tab MUST become active. The PTY is spawned by `SingleTerminal` on
 * mount, and a pane only mounts its active tab — so `addTabBackground` here
 * would return a terminal id whose PTY never comes into existence, and the
 * caller would block until the spawn timeout.
 */
async function handleTerminalSpawn(payload: {
	request_id: string;
	cwd?: string | null;
	argv?: string[] | null;
	title?: string | null;
	pane?: string | null;
}) {
	const reply = (result: { terminal_id?: string; error?: string }) =>
		invoke('iyke_terminal_spawn_done', {
			requestId: payload.request_id,
			result: { terminal_id: result.terminal_id ?? null, error: result.error ?? null },
		}).catch((err) => {
			console.error('[iyke] iyke_terminal_spawn_done failed:', err);
		});

	try {
		const store = usePaneStore.getState();
		const leafId = payload.pane ?? store.focusedId;
		const leaf = findLeaf(store.root, leafId);
		if (!leaf) {
			await reply({ error: `pane not found: ${leafId}` });
			return;
		}
		const sessionId = createTerminalSession({
			cwd: payload.cwd ?? undefined,
			cmd: payload.argv ?? undefined,
			title: payload.title ?? undefined,
		});
		// Activating is load-bearing, not cosmetic — see the note above.
		store.addTab(leaf.id, { kind: 'terminal', sessionId });
		await reply({ terminal_id: sessionId });
	} catch (err) {
		await reply({ error: err instanceof Error ? err.message : String(err) });
	}
}

/** WP-08 — drop the tab for a terminal the bridge killed, when the caller
 *  asked for it. Fire-and-forget: the process is already dead, so missing this
 *  just leaves a harmless exited tab behind. */
function handleTerminalCloseTab(payload: { terminal_id: string }) {
	const store = usePaneStore.getState();
	const visit = (node: PaneNode): boolean => {
		if (node.type === 'leaf') {
			const idx = node.tabs.findIndex(
				(t) => t.kind === 'terminal' && t.sessionId === payload.terminal_id
			);
			if (idx >= 0) {
				store.closeTab(node.id, idx);
				return true;
			}
			return false;
		}
		return node.children.some(visit);
	};
	visit(store.root);
}

async function handleTerminalSend(payload: TerminalSendPayload) {
	const reply = (matched: boolean) =>
		payload.request_id
			? invoke('iyke_action_done', {
					requestId: payload.request_id,
					result: { matched },
				}).catch((err) => {
					console.error('[iyke] iyke_action_done failed:', err);
				})
			: Promise.resolve();
	const sessionId = resolveTerminalSessionId(payload.pane ?? null);
	const actId = useIykeActivity.getState().begin({
		kind: 'type',
		scope: payload.pane ?? 'focused',
		detail: 'terminal-send',
	});
	try {
		if (!sessionId) {
			console.warn('[iyke] terminal-send: pane has no active terminal tab', payload.pane);
			await reply(false);
			return;
		}
		const pty = getPty(sessionId);
		if (!pty) {
			console.warn('[iyke] terminal-send: pty not registered for session', sessionId);
			await reply(false);
			return;
		}
		let buf = '';
		if (payload.data) buf += payload.data;
		for (const combo of payload.keys ?? []) {
			const bytes = comboToTerminalBytes(combo);
			if (bytes === null) {
				console.warn('[iyke] terminal-send: unknown key combo', combo);
				continue;
			}
			buf += bytes;
		}
		if (buf.length === 0) {
			await reply(false);
			return;
		}
		await pty.write(buf);
		await reply(true);
	} finally {
		useIykeActivity.getState().end(actId);
	}
}

function handleKey(payload: KeyPayload) {
	const reg = resolveIframeReg(payload.pane);
	const directDoc = reg ? directDocFor(reg) : null;
	if (reg && !directDoc) {
		const id = useIykeActivity.getState().begin({
			kind: 'key',
			scope: reg.paneId,
			detail: payload.combo,
		});
		postToIframeFireAndForget(reg.paneId, 'key', {
			combo: payload.combo,
			ref: payload.ref ?? null,
			selector: payload.selector ?? null,
		});
		useIykeActivity.getState().end(id);
		return;
	}
	const fallbackDoc = directDoc ?? document;
	const target =
		resolveTarget(payload.ref, payload.selector, null, directDoc ?? undefined) ??
		fallbackDoc.activeElement ??
		fallbackDoc.body;
	if (!target) return;
	const id = useIykeActivity.getState().begin({
		kind: 'key',
		scope: resolvePaneScope(target instanceof Element ? target : null),
		detail: payload.combo,
	});
	const combo = parseCombo(payload.combo);
	for (const type of ['keydown', 'keypress', 'keyup'] as const) {
		if (type === 'keypress' && combo.key.length !== 1) continue;
		target.dispatchEvent(
			new KeyboardEvent(type, {
				key: combo.key,
				code: combo.key,
				ctrlKey: combo.ctrl,
				altKey: combo.alt,
				shiftKey: combo.shift,
				metaKey: combo.meta,
				bubbles: true,
				cancelable: true,
			})
		);
	}
	useIykeActivity.getState().end(id);
}

// ── Hook ─────────────────────────────────────────────────────────────────

export function useIykeBridge(): void {
	useEffect(() => {
		installIykeIframeMessageListener();

		const unlisteners: UnlistenFn[] = [];
		let cancelled = false;
		const track = (p: Promise<UnlistenFn>) =>
			p
				.then((u) => {
					if (cancelled) u();
					else unlisteners.push(u);
				})
				.catch((err) => {
					console.error('[iyke] subscribe failed:', err);
				});

		track(listen<DomRequestPayload>('iyke://dom-request', (e) => handleDomRequest(e.payload)));
		track(
			listen<{ request_id: string; pane: string }>('iyke://iframe-state-request', (e) => {
				// resolveIframeReg also maps pane-leaf ids (from `iyke state`) whose
				// active tab is a /pkg/<pkgId>/ route to that pkg's registration.
				const reg = resolveIframeReg(e.payload.pane);
				const state = reg ? reg.state : null;
				invoke('iyke_dom_done', {
					requestId: e.payload.request_id,
					result: {
						text: JSON.stringify(state, null, 2),
						json: state ?? {},
						generation: currentStateGeneration(),
					},
				}).catch((err) => {
					console.error('[iyke] iyke_dom_done (iframe-state) failed:', err);
				});
			})
		);
		track(
			listen<{ pane: string; kind: string; payload: unknown }>('iyke://iframe-message', (e) => {
				postToIframeFireAndForget(e.payload.pane, e.payload.kind, e.payload.payload);
			})
		);
		track(
			listen<QueryCacheRequestPayload>('iyke://query-cache-request', (e) =>
				handleQueryCacheRequest(e.payload)
			)
		);
		track(
			listen<WaitRequestPayload>('iyke://wait-request', (e) => {
				void handleWaitRequest(e.payload);
			})
		);
		track(listen<ClickPayload>('iyke://click', (e) => void handleClick(e.payload)));
		track(listen<TypePayload>('iyke://type', (e) => handleType(e.payload)));
		track(listen<KeyPayload>('iyke://key', (e) => handleKey(e.payload)));
		track(
			listen<{
				request_id: string;
				cwd?: string | null;
				argv?: string[] | null;
				title?: string | null;
				pane?: string | null;
			}>('iyke://terminal-spawn', (e) => void handleTerminalSpawn(e.payload))
		);
		track(
			listen<{ terminal_id: string }>('iyke://terminal-close-tab', (e) =>
				handleTerminalCloseTab(e.payload)
			)
		);
		track(
			listen<TerminalSendPayload>('iyke://terminal-send', (e) => {
				void handleTerminalSend(e.payload);
			})
		);
		track(
			listen<TerminalReadPayload>('iyke://terminal-read-request', (e) => {
				void handleTerminalReadRequest(e.payload);
			})
		);
		track(
			listen<{ pane: string; index?: number | null; terminal?: string | null }>(
				'iyke://tab-activate',
				(e) => {
					const store = usePaneStore.getState();
					const leaf = findLeaf(store.root, e.payload.pane);
					if (!leaf) return;
					const index =
						e.payload.index ??
						leaf.tabs.findIndex(
							(tab) => tab.kind === 'terminal' && tab.sessionId === e.payload.terminal
						);
					if (index >= 0 && index < leaf.tabs.length) store.switchTab(leaf.id, index);
				}
			)
		);

		return () => {
			cancelled = true;
			for (const u of unlisteners) u();
		};
	}, []);
}
