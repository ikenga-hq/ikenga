import { toast } from '@/lib/toast';
import { getTransport, isBrowserHost, isTauri } from './index';

export type UnlistenFn = () => void;

/**
 * Common desktop-vs-browser shims for @tauri-apps/* APIs.
 *
 * Every non-transport Tauri API that the shell uses must be imported through
 * this file so that the browser path can degrade gracefully. Direct imports of
 * @tauri-apps/* outside lib/transport break G-TRANSPORT sign-off.
 */

const EXTERNAL_URL_PROTOCOLS = new Set(['http:', 'https:', 'mailto:']);

/** True for an absolute `http(s):` or `mailto:` URL — the only things
 *  {@link openExternalUrl} will open. */
export function isExternalUrl(url: string): boolean {
	try {
		return EXTERNAL_URL_PROTOCOLS.has(new URL(url).protocol);
	} catch {
		return false;
	}
}

/**
 * Open a web address (or mail link) in the user's browser.
 *
 * Accepts only `http(s):` and `mailto:` and rejects anything else — notably a
 * filesystem path. In a browser session `window.open(path)` resolves the path
 * against the Ikenga origin and opens a junk Ikenga tab, and the server's
 * paths mean nothing to the browser anyway. Files and folders go through
 * {@link openLocalPath}.
 */
export async function openExternalUrl(url: string): Promise<void> {
	if (!isExternalUrl(url)) {
		const shown = url.length > 80 ? `${url.slice(0, 80)}…` : url;
		throw new Error(`Only http(s) and mailto links can be opened (got “${shown}”).`);
	}
	if (isTauri()) {
		try {
			const { open } = await import('@tauri-apps/plugin-shell');
			await open(url);
			return;
		} catch (e) {
			console.warn('Tauri open plugin error, falling back to window.open', e);
		}
	}
	window.open(url, '_blank', 'noopener,noreferrer');
}

/** Whether "open this path in the default app" can do anything here. On the
 *  desktop it always can; a browser session can only download a file — a
 *  folder on the server has no browser equivalent, so its control is hidden. */
export function canOpenLocalPath(kind: 'file' | 'folder'): boolean {
	return !isBrowserHost() || kind === 'file';
}

function baseName(path: string): string {
	const parts = path.split(/[\\/]/).filter(Boolean);
	return parts[parts.length - 1] ?? 'download';
}

/** Hand a Blob to the browser as a file download. */
export function saveBlobAs(blob: Blob, filename: string): void {
	const url = URL.createObjectURL(blob);
	const a = document.createElement('a');
	a.href = url;
	a.download = filename;
	a.rel = 'noopener';
	document.body.appendChild(a);
	a.click();
	a.remove();
	// Give the browser a tick to start the download before releasing the blob.
	setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

/** Download a file that lives on the server through the served `fs_read`. */
export async function downloadServerFile(path: string): Promise<void> {
	const res = await getTransport().invoke<{ bytes: number[]; mime?: string }>('fs_read', { path });
	const blob = new Blob([new Uint8Array(res.bytes)], {
		type: res.mime || 'application/octet-stream',
	});
	saveBlobAs(blob, baseName(path));
}

/**
 * "Open in default app" / "Open folder" / "Open file" / "Reveal in Files".
 *
 * Desktop: the OS opens `path` with its default handler. Browser session: the
 * path is on the server, so the nearest honest equivalent is downloading the
 * file; a folder cannot be opened and rejects (callers hide the control via
 * {@link canOpenLocalPath}, this is the backstop).
 */
export async function openLocalPath(
	path: string,
	opts: { kind?: 'file' | 'folder' } = {}
): Promise<void> {
	if (isBrowserHost()) {
		if (opts.kind === 'folder') {
			throw new Error('Folders on the server cannot be opened from a browser.');
		}
		await downloadServerFile(path);
		return;
	}
	// tauri-plugin-shell's `open` shells out to xdg-open / `open` /
	// explorer.exe — the OS picks the default handler.
	const { open } = await import('@tauri-apps/plugin-shell');
	await open(path);
}

/**
 * Thrown when the clipboard cannot be reached. The usual cause is a browser
 * session on an insecure origin (plain HTTP on a tailnet), where
 * `navigator.clipboard` is `undefined`; a denied permission lands here too.
 * Callers tell the user instead of pretending the copy/paste happened.
 */
export class ClipboardUnavailableError extends Error {
	readonly operation: 'read' | 'write';

	constructor(operation: 'read' | 'write', detail?: string) {
		super(
			operation === 'read'
				? `Clipboard read is unavailable${detail ? `: ${detail}` : ''}`
				: `Clipboard write is unavailable${detail ? `: ${detail}` : ''}`
		);
		this.name = 'ClipboardUnavailableError';
		this.operation = operation;
	}
}

/**
 * Legacy copy path for contexts without the async clipboard API (insecure
 * origins). Needs a user gesture, which every Copy action has. Restores focus
 * and the selection afterwards so the terminal keeps its caret.
 */
function execCommandCopy(text: string): boolean {
	if (typeof document === 'undefined' || typeof document.execCommand !== 'function') return false;
	const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
	const area = document.createElement('textarea');
	area.value = text;
	area.setAttribute('readonly', '');
	area.setAttribute('aria-hidden', 'true');
	area.style.cssText = 'position:fixed;top:0;left:-9999px;opacity:0;pointer-events:none;';
	document.body.appendChild(area);
	try {
		area.focus({ preventScroll: true });
		area.select();
		area.setSelectionRange(0, text.length);
		return document.execCommand('copy');
	} catch {
		return false;
	} finally {
		area.remove();
		active?.focus({ preventScroll: true });
	}
}

export async function writeClipboardText(text: string): Promise<void> {
	if (isTauri()) {
		try {
			const { writeText } = await import('@tauri-apps/plugin-clipboard-manager');
			await writeText(text);
			return;
		} catch (e) {
			console.warn('Tauri clipboard plugin error, falling back to navigator.clipboard', e);
		}
	}
	let cause = 'navigator.clipboard is unavailable (insecure origin?)';
	if (navigator.clipboard?.writeText) {
		try {
			await navigator.clipboard.writeText(text);
			return;
		} catch (e) {
			cause = e instanceof Error ? e.message : String(e);
		}
	}
	if (execCommandCopy(text)) return;
	throw new ClipboardUnavailableError('write', cause);
}

export async function readClipboardText(): Promise<string> {
	if (isTauri()) {
		try {
			const { readText } = await import('@tauri-apps/plugin-clipboard-manager');
			return await readText();
		} catch (e) {
			console.warn('Tauri clipboard plugin error, falling back to navigator.clipboard', e);
		}
	}
	if (!navigator.clipboard?.readText) {
		throw new ClipboardUnavailableError(
			'read',
			'navigator.clipboard is unavailable (insecure origin?)'
		);
	}
	try {
		return await navigator.clipboard.readText();
	} catch (e) {
		throw new ClipboardUnavailableError('read', e instanceof Error ? e.message : String(e));
	}
}

// ─── Core transport helpers ──────────────────────────────────────────────────

export function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	return getTransport().invoke<T>(cmd, args);
}

export function listen<T>(
	event: string,
	handler: (event: { event: string; payload: T }) => void
): Promise<UnlistenFn> {
	return getTransport().listen<T>(event, handler);
}

export async function emit(event: string, payload?: unknown): Promise<void> {
	if (isTauri()) {
		try {
			const { emit } = await import('@tauri-apps/api/event');
			await emit(event, payload);
			return;
		} catch (e) {
			console.warn(`Tauri emit('${event}') failed`, e);
		}
	}
	console.warn(`[transport] emit('${event}') has no backend event bus in browser mode`);
}

// ─── Notifications ───────────────────────────────────────────────────────────

export interface NotificationOptions {
	title: string;
	body?: string;
	icon?: string;
}

/** True when this page can construct a `Notification` at all. iOS Safari
 *  outside an installed web app has no `Notification` global. */
function browserNotificationApi(): typeof Notification | null {
	return typeof Notification === 'undefined' ? null : Notification;
}

/** Whether OS notifications can be raised at all (desktop: always). */
export function notificationsSupported(): boolean {
	return isTauri() || browserNotificationApi() !== null;
}

/** The browser's permission state, or `'unsupported'` when there is no
 *  `Notification` API. Never prompts. */
export function browserNotificationPermission(): NotificationPermission | 'unsupported' {
	return browserNotificationApi()?.permission ?? 'unsupported';
}

export async function isNotificationPermissionGranted(): Promise<boolean> {
	if (isTauri()) {
		try {
			const { isPermissionGranted } = await import('@tauri-apps/plugin-notification');
			return await isPermissionGranted();
		} catch (e) {
			console.warn('Tauri notification permission check failed', e);
		}
	}
	return browserNotificationApi()?.permission === 'granted';
}

/**
 * Ask for OS notification permission.
 *
 * In a browser the prompt is only honoured from a user gesture (Firefox and
 * Safari never grant otherwise, and Chrome throttles it), so outside a click
 * this returns the current state without prompting. Call it from an explicit
 * "Enable notifications" control; everything else falls back to a toast.
 */
export async function requestNotificationPermission(): Promise<NotificationPermission | string> {
	if (isTauri()) {
		try {
			const { requestPermission } = await import('@tauri-apps/plugin-notification');
			return await requestPermission();
		} catch (e) {
			console.warn('Tauri notification permission request failed', e);
		}
	}
	const api = browserNotificationApi();
	if (!api) return 'denied';
	if (api.permission !== 'default') return api.permission;
	if (!navigator.userActivation?.isActive) return api.permission;
	try {
		return await api.requestPermission();
	} catch {
		return api.permission;
	}
}

function toastNotification(title: string, body?: string): void {
	toast({ label: body ? `${title} — ${body}` : title, variant: 'notice' });
}

/**
 * Raise an OS notification; where that is impossible (no `Notification` API,
 * permission not granted, or a constructor that throws — Android Chrome
 * rejects `new Notification` outside a service worker) show a toast instead,
 * so the message is never silently lost.
 */
export async function sendNotification(options: NotificationOptions | string): Promise<void> {
	if (isTauri()) {
		try {
			const { sendNotification } = await import('@tauri-apps/plugin-notification');
			sendNotification(options);
			return;
		} catch (e) {
			console.warn('Tauri sendNotification failed', e);
		}
	}
	const opts = typeof options === 'string' ? { title: options } : options;
	const api = browserNotificationApi();
	if (api && api.permission === 'granted') {
		try {
			new api(opts.title, { body: opts.body, icon: opts.icon });
			return;
		} catch (e) {
			console.warn('Notification constructor unavailable, falling back to toast', e);
		}
	}
	toastNotification(opts.title, opts.body);
}

// ─── App version ─────────────────────────────────────────────────────────────

export async function getAppVersion(): Promise<string> {
	if (isTauri()) {
		try {
			const { getVersion } = await import('@tauri-apps/api/app');
			return await getVersion();
		} catch (e) {
			console.warn('Tauri getVersion failed', e);
		}
	}
	// Browser: best effort from the daemon health endpoint; fallback to zero.
	try {
		const res = await fetch('/api/health');
		if (res.ok) {
			const data = await res.json();
			if (data.version) return data.version;
		}
	} catch {
		// ignored
	}
	return '0.0.0';
}

// ─── Window / webview / menu (desktop-only) ──────────────────────────────────

export type WebviewWindow = any;

function makeLazyWindow(): any {
	return {
		setTitle: async (title: string) => {
			if (!isTauri()) return;
			try {
				const { getCurrentWindow } = await import('@tauri-apps/api/window');
				const w = getCurrentWindow();
				await w.setTitle(title);
			} catch (e) {
				console.warn('[transport] getCurrentWindow().setTitle failed', e);
			}
		},
	};
}

export function getCurrentWindow(): any | null {
	if (isTauri()) return makeLazyWindow();
	return null;
}

function makeLazyWebview(): any {
	return {
		setZoom: async (level: number) => {
			if (!isTauri()) return;
			try {
				const { getCurrentWebview } = await import('@tauri-apps/api/webview');
				const w = getCurrentWebview();
				await w.setZoom(level);
			} catch (e) {
				console.warn('[transport] getCurrentWebview().setZoom failed', e);
			}
		},
		onDragDropEvent: async (handler: (event: any) => void) => {
			if (!isTauri()) return () => {};
			try {
				const { getCurrentWebview } = await import('@tauri-apps/api/webview');
				const w = getCurrentWebview();
				return await w.onDragDropEvent(handler);
			} catch (e) {
				console.warn('[transport] getCurrentWebview().onDragDropEvent failed', e);
				return () => {};
			}
		},
	};
}

export function getCurrentWebview(): any | null {
	if (isTauri()) return makeLazyWebview();
	return null;
}

export async function setWindowTitle(title: string): Promise<void> {
	if (!isTauri()) return;
	try {
		const { getCurrentWindow } = await import('@tauri-apps/api/window');
		const w = getCurrentWindow();
		await w.setTitle(title);
	} catch (e) {
		console.warn('setWindowTitle failed', e);
	}
}

export async function showApplicationMenu(_template?: unknown): Promise<void> {
	if (!isTauri()) return;
	// Native application menu wiring is desktop-only and handled by
	// `shell/native-menu.ts`. This stub exists for import-routing symmetry.
	console.warn('[transport] showApplicationMenu is desktop-only; skipping', _template);
}

// ─── Updater / process (desktop-only) ────────────────────────────────────────

export interface UpdateInfo {
	version: string;
	date?: string;
	notes?: string;
	currentVersion?: string;
	/** Opaque plugin handle; present only in the Tauri runtime. */
	handle?: any;
}

export async function checkForUpdate(): Promise<UpdateInfo | null> {
	if (!isTauri()) {
		console.warn('[transport] Updater is desktop-only in browser mode');
		return null;
	}
	try {
		const { check } = await import('@tauri-apps/plugin-updater');
		const update = await check();
		if (update?.available) {
			return {
				version: update.version,
				date: update.date,
				notes: update.body,
				currentVersion: update.currentVersion,
				handle: update,
			};
		}
		return null;
	} catch (e) {
		console.warn('Tauri updater check failed', e);
		return null;
	}
}

export async function relaunchApp(): Promise<void> {
	if (!isTauri()) {
		console.warn('[transport] relaunchApp is desktop-only in browser mode');
		return;
	}
	try {
		const { relaunch } = await import('@tauri-apps/plugin-process');
		await relaunch();
	} catch (e) {
		console.warn('Tauri relaunch failed', e);
	}
}
