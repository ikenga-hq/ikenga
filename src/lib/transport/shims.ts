import { isTauri } from './index';

/**
 * Common desktop-vs-browser shims for @tauri-apps/plugin-* APIs.
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
			console.warn('Tauri notification plugin error', e);
		}
	}
	if (typeof window !== 'undefined' && 'Notification' in window) {
		return Notification.permission === 'granted';
	}
	return false;
}

export async function requestNotificationPermission(): Promise<string> {
	if (isTauri()) {
		try {
			const { requestPermission } = await import('@tauri-apps/plugin-notification');
			return await requestPermission();
		} catch (e) {
			console.warn('Tauri notification plugin error', e);
		}
	}
	if (typeof window !== 'undefined' && 'Notification' in window) {
		return await Notification.requestPermission();
	}
	return 'denied';
}

export async function sendDesktopNotification(options: NotificationOptions): Promise<void> {
	if (isTauri()) {
		try {
			const { sendNotification } = await import('@tauri-apps/plugin-notification');
			sendNotification(options);
			return;
		} catch (e) {
			console.warn('Tauri notification plugin error, falling back to Web Notification', e);
		}
	}
	if (typeof window !== 'undefined' && 'Notification' in window && Notification.permission === 'granted') {
		try {
			new Notification(options.title, { body: options.body, icon: options.icon });
		} catch {
			// Ignored if notifications are blocked or fail
		}
	}
}

