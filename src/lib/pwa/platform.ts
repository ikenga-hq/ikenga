// plans/pwa S1 + the install part of Shape 4: what this browser can do about
// installing Ikenga, so the UI states it honestly instead of offering a button
// that silently does nothing.
//
// - Install, the service worker and Web Push all need a secure context.
//   Plain-HTTP LAN or tailnet access gets none of them.
// - iOS/iPadOS have no install prompt: Share › Add to Home Screen. A Home
//   Screen app also has storage separate from Safari, so it must be paired
//   from inside the installed app.
// - A T0 bearer-token tab keeps its token in tab-scoped sessionStorage, which
//   an installed app does not keep across launches. Pairing (an HttpOnly
//   device cookie) survives.

export type InstallState =
	/** The desktop app. Nothing to install. */
	| 'tauri'
	/** Already running as an installed app. */
	| 'installed'
	/** Not HTTPS/localhost: no install, no worker, no push. */
	| 'insecure'
	/** iOS/iPadOS Safari: install from the Share sheet by hand. */
	| 'ios-manual'
	/** The browser handed us an install prompt to show on a click. */
	| 'prompt'
	/** Installable from the browser's own menu, or not at all. */
	| 'browser-menu';

export interface PlatformEnv {
	isTauri: boolean;
	secureContext: boolean;
	standalone: boolean;
	userAgent: string;
	/** `navigator.platform` (iPadOS reports `MacIntel`). */
	platform: string;
	maxTouchPoints: number;
	hasInstallPrompt: boolean;
}

/** iPhone/iPod/iPad, including iPadOS 13+ that presents as a Mac. */
export function isIos(
	env: Pick<PlatformEnv, 'userAgent' | 'platform' | 'maxTouchPoints'>
): boolean {
	if (/iPhone|iPad|iPod/i.test(env.userAgent)) return true;
	return env.platform === 'MacIntel' && env.maxTouchPoints > 1;
}

export function installState(env: PlatformEnv): InstallState {
	if (env.isTauri) return 'tauri';
	if (env.standalone) return 'installed';
	if (!env.secureContext) return 'insecure';
	if (isIos(env)) return 'ios-manual';
	if (env.hasInstallPrompt) return 'prompt';
	return 'browser-menu';
}

/** True when the page runs as an installed app (any platform). */
export function isStandaloneDisplay(): boolean {
	if (typeof window === 'undefined') return false;
	try {
		if (window.matchMedia?.('(display-mode: standalone)').matches) return true;
	} catch {
		// Old engines throw on unknown media features.
	}
	return (navigator as Navigator & { standalone?: boolean }).standalone === true;
}

/** The one line the UI shows for each state, or `null` when there is nothing to say. */
export function installMessage(state: InstallState): string | null {
	switch (state) {
		case 'insecure':
			return 'Installing Ikenga and notifications need HTTPS. Open Ikenga at an https:// address (for example through tailscale serve, with IKENGA_PUBLIC_URL set to it).';
		case 'ios-manual':
			return 'To install, tap Share, then Add to Home Screen. Open Ikenga from the Home Screen and pair it there: the installed app does not share Safari’s sign-in.';
		case 'prompt':
			return 'Install Ikenga as an app for a full-screen window and a Home Screen icon.';
		case 'browser-menu':
			return 'You can install Ikenga from your browser’s menu (Install app / Add to Home screen).';
		default:
			return null;
	}
}

/**
 * The T0 caveat: an installed app signed in with a link token loses it when
 * closed. `null` when it doesn't apply.
 */
export function tokenSessionWarning(opts: {
	standalone: boolean;
	hasBearerToken: boolean;
	isDevice: boolean;
	isT1: boolean;
}): string | null {
	if (!opts.standalone || !opts.hasBearerToken || opts.isDevice || opts.isT1) return null;
	return 'This installed app is signed in with a link token, which it forgets when closed. Pair this device to stay signed in.';
}
