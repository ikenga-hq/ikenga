// plans/pwa S1: the browser-app lifecycle state the PWA chrome reads — a new
// service worker waiting to take over ("Reload to update"), and a captured
// install prompt. Written only by `register.ts`; read by `PwaUpdateBanner`
// and `InstallHint`. Never populated under Tauri.

import { create } from 'zustand';

/** Chromium's `beforeinstallprompt` event (not in the DOM lib). */
export interface InstallPromptEvent extends Event {
	prompt(): Promise<void>;
	readonly userChoice: Promise<{ outcome: 'accepted' | 'dismissed' }>;
}

interface PwaState {
	/** A new worker installed and waiting for this page to hand over. */
	waiting: ServiceWorker | null;
	/** The deferred install prompt, when the browser offered one. */
	installPrompt: InstallPromptEvent | null;
	setWaiting: (sw: ServiceWorker | null) => void;
	setInstallPrompt: (e: InstallPromptEvent | null) => void;
}

export const usePwaStore = create<PwaState>((set) => ({
	waiting: null,
	installPrompt: null,
	setWaiting: (waiting) => set({ waiting }),
	setInstallPrompt: (installPrompt) => set({ installPrompt }),
}));
