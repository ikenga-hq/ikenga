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

// ── Push (plans/pwa S4 §2, Shape 4) ─────────────────────────────────────────

/** What this browser can do about push notifications — exactly one state. */
export type PushState =
	/** The desktop app: no service worker, no push. */
	| 'tauri'
	/** Not HTTPS/localhost: no worker, no push. */
	| 'insecure'
	/** iOS/iPadOS Safari tab: Web Push reaches only a Home Screen app (16.4+). */
	| 'ios-needs-install'
	/** No PushManager / service worker / Notification in this browser. */
	| 'unsupported'
	/** The user blocked notifications for this site. */
	| 'denied'
	| 'ready';

export interface PushEnv {
	isTauri: boolean;
	secureContext: boolean;
	standalone: boolean;
	userAgent: string;
	platform: string;
	maxTouchPoints: number;
	hasServiceWorker: boolean;
	hasPushManager: boolean;
	/** `Notification.permission`, or `'unsupported'` with no Notification API. */
	permission: NotificationPermission | 'unsupported';
}

export function pushState(env: PushEnv): PushState {
	if (env.isTauri) return 'tauri';
	if (!env.secureContext) return 'insecure';
	// Before `unsupported`: an iOS Safari tab has no PushManager at all, and
	// the useful thing to say is "install it", not "can't".
	if (isIos(env) && !env.standalone) return 'ios-needs-install';
	if (!env.hasServiceWorker || !env.hasPushManager || env.permission === 'unsupported') {
		return 'unsupported';
	}
	if (env.permission === 'denied') return 'denied';
	return 'ready';
}

/** The one line the UI shows for a push state, or `null` for `ready`. */
export function pushMessage(state: PushState): string | null {
	switch (state) {
		case 'tauri':
			return 'Notifications reach your phone or another browser: turn them on from Ikenga opened in that browser.';
		case 'insecure':
			return 'Notifications need HTTPS. Open Ikenga at an https:// address (set IKENGA_PUBLIC_URL, or use tailscale serve).';
		case 'ios-needs-install':
			return 'On iPhone and iPad, notifications only reach Ikenga installed to the Home Screen (iOS 16.4 or later): tap Share, then Add to Home Screen, open Ikenga from there, and pair this device inside the installed app.';
		case 'unsupported':
			return 'This browser can’t receive push notifications.';
		case 'denied':
			return 'Notifications are blocked for this site. Allow them in the browser’s site settings, then come back here.';
		default:
			return null;
	}
}

/** This page's push environment. */
export function currentPushEnv(isTauri: boolean): PushEnv {
	const nav = typeof navigator === 'undefined' ? null : navigator;
	const win = typeof window === 'undefined' ? null : window;
	return {
		isTauri,
		secureContext: win?.isSecureContext === true,
		standalone: isStandaloneDisplay(),
		userAgent: nav?.userAgent ?? '',
		platform: nav?.platform ?? '',
		maxTouchPoints: nav?.maxTouchPoints ?? 0,
		hasServiceWorker: nav !== null && 'serviceWorker' in nav,
		hasPushManager: win !== null && 'PushManager' in win,
		permission:
			win !== null && 'Notification' in win
				? (win as Window & { Notification: typeof Notification }).Notification.permission
				: 'unsupported',
	};
}
