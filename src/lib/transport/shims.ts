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
