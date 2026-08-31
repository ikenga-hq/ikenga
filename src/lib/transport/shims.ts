import { getTransport, isTauri } from './index';

export type UnlistenFn = () => void;

/**
 * Common desktop-vs-browser shims for @tauri-apps/* APIs.
 *
 * Every non-transport Tauri API that the shell uses must be imported through
 * this file so that the browser path can degrade gracefully. Direct imports of
 * @tauri-apps/* outside lib/transport break G-TRANSPORT sign-off.
 */

export async function openExternalUrl(url: string): Promise<void> {
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
	if (navigator.clipboard) {
		await navigator.clipboard.writeText(text);
	}
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
	if (navigator.clipboard) {
		return await navigator.clipboard.readText();
	}
	return '';
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

export async function isNotificationPermissionGranted(): Promise<boolean> {
	if (isTauri()) {
		try {
			const { isPermissionGranted } = await import('@tauri-apps/plugin-notification');
			return await isPermissionGranted();
		} catch (e) {
			console.warn('Tauri notification permission check failed', e);
		}
	}
	return Notification.permission === 'granted';
}

export async function requestNotificationPermission(): Promise<NotificationPermission | string> {
	if (isTauri()) {
		try {
			const { requestPermission } = await import('@tauri-apps/plugin-notification');
			return await requestPermission();
		} catch (e) {
			console.warn('Tauri notification permission request failed', e);
		}
	}
	return Notification.requestPermission();
}

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
	if (typeof options === 'string') {
		new Notification(options);
	} else {
		new Notification(options.title, { body: options.body, icon: options.icon });
	}
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
