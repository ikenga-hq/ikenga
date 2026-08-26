import { isTauri } from './index';
import { useDialogStore, type OpenDialogOptions } from './dialog-store';

export type { OpenDialogOptions };

export interface ConfirmOptions {
	title?: string;
	kind?: 'info' | 'warning' | 'error';
	okLabel?: string;
	cancelLabel?: string;
}

export interface MessageOptions {
	title?: string;
	kind?: 'info' | 'warning' | 'error';
	okLabel?: string;
}

export async function open(options: OpenDialogOptions = {}): Promise<string | string[] | null> {
	if (isTauri()) {
		try {
			const mod = await import('@tauri-apps/plugin-dialog');
			return await mod.open(options);
		} catch (e) {
			console.warn('[dialog-shim] Tauri open dialog failed, using web path picker', e);
		}
	}
	return useDialogStore.getState().requestOpen(options);
}

export async function save(options: OpenDialogOptions = {}): Promise<string | null> {
	if (isTauri()) {
		try {
			const mod = await import('@tauri-apps/plugin-dialog');
			return await mod.save(options);
		} catch (e) {
			console.warn('[dialog-shim] Tauri save dialog failed, using web path picker', e);
		}
	}
	return useDialogStore.getState().requestSave(options);
}

export async function confirm(
	messageText: string,
	options?: ConfirmOptions | string
): Promise<boolean> {
	if (isTauri()) {
		try {
			const mod = await import('@tauri-apps/plugin-dialog');
			return await mod.confirm(messageText, options as any);
		} catch (e) {
			console.warn('[dialog-shim] Tauri confirm dialog failed, falling back to window.confirm', e);
		}
	}
	// `window.confirm` is absent under jsdom and can be disabled by the
	// embedder; it returns `undefined` in both cases rather than throwing, so
	// the result is coerced instead of trusted. A confirm that cannot be shown
	// must read as "not confirmed" — never as an accidental yes.
	if (typeof window !== 'undefined' && typeof window.confirm === 'function') {
		try {
			return window.confirm(messageText) === true;
		} catch {
			return false;
		}
	}
	return false;
}

export async function message(
	messageText: string,
	options?: MessageOptions | string
): Promise<void> {
	if (isTauri()) {
		try {
			const mod = await import('@tauri-apps/plugin-dialog');
			await mod.message(messageText, options as any);
			return;
		} catch (e) {
			console.warn('[dialog-shim] Tauri message dialog failed, falling back to window.alert', e);
		}
	}
	if (typeof window !== 'undefined' && typeof window.alert === 'function') {
		try {
			window.alert(messageText);
			return;
		} catch {
			// Fall through to the log below.
		}
	}
	// Better in the console than nowhere — a message the user was meant to see
	// should not vanish because the host blocks modal dialogs.
	console.warn(`[dialog-shim] ${messageText}`);
}

export async function ask(
	messageText: string,
	options?: ConfirmOptions | string
): Promise<boolean> {
	return confirm(messageText, options);
}
