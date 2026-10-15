// plans/pwa S1 (W1/W2): register the service worker for the browser-served
// app, and run the "Reload to update" hand-over.
//
// Registration happens only when ALL of these hold:
//   - not Tauri — the desktop app embeds the same dist but must never run a
//     worker (it would cache the desktop's own bundle across app updates);
//   - a production build — Vite dev serves no `/sw.js` and must stay uncached;
//   - the browser has `navigator.serviceWorker`;
//   - a secure context — HTTPS or localhost. Plain-HTTP LAN/tailnet gets
//     nothing, and `InstallHint` says why.
//
// Update flow: the browser checks `/sw.js` (served `no-cache`, registered with
// `updateViaCache: 'none'`) on navigation, on `visibilitychange` and hourly.
// A new worker installs and WAITS; `usePwaStore.waiting` raises the banner;
// `applyUpdate()` posts `SKIP_WAITING` and the page reloads once on the
// resulting `controllerchange`. The first install's `clients.claim()` also
// fires `controllerchange`, which must NOT reload — hence `reloadRequested`.
//
// Two ways the hand-over can find the world already moved on, both handled:
//   - the stored worker is no longer waiting (another tab applied the update,
//     or it activated by itself): `SKIP_WAITING` would be a no-op and no
//     `controllerchange` would ever come, so `applyUpdate()` just reloads;
//   - a `controllerchange` this tab did not ask for (another tab updated):
//     this tab runs old code under a new worker. It is NOT reloaded — the
//     user may be mid-work — but the banner is raised so they can.
// And if `SKIP_WAITING` is posted yet no `controllerchange` follows, the
// reload happens anyway after `HANDOVER_TIMEOUT_MS`.
//
// The reload itself is a deliberate one: it steps past the terminal "Leave
// site?" prompt (see `withTerminalGuardSuspended`) but NOT past the
// unsaved-edits one.

import { isTauri } from '@/lib/transport';
import {
	_resetUnloadSuspendForTests,
	withTerminalGuardSuspended,
} from '@/lib/window/unload-suspend';
import { SKIP_WAITING_MESSAGE } from './sw-logic';
import { type InstallPromptEvent, usePwaStore } from './update-store';

export const SW_URL = '/sw.js';
const UPDATE_INTERVAL_MS = 60 * 60 * 1000;
/** How long to wait for `controllerchange` after `SKIP_WAITING` before reloading anyway. */
export const HANDOVER_TIMEOUT_MS = 3000;

export type RegistrationBlocker = 'tauri' | 'dev' | 'unsupported' | 'insecure';

export interface RegistrationEnv {
	isTauri: boolean;
	prod: boolean;
	hasServiceWorker: boolean;
	secureContext: boolean;
}

/** Why this page must not register a worker, or `null` when it may. */
export function registrationBlocker(env: RegistrationEnv): RegistrationBlocker | null {
	if (env.isTauri) return 'tauri';
	if (!env.prod) return 'dev';
	if (!env.hasServiceWorker) return 'unsupported';
	if (!env.secureContext) return 'insecure';
	return null;
}

export function currentRegistrationEnv(): RegistrationEnv {
	const hasWindow = typeof window !== 'undefined';
	return {
		isTauri: isTauri(),
		prod: import.meta.env.PROD === true,
		hasServiceWorker: hasWindow && typeof navigator !== 'undefined' && 'serviceWorker' in navigator,
		secureContext: hasWindow && window.isSecureContext === true,
	};
}

let reloadRequested = false;
let handoverTimer: ReturnType<typeof setTimeout> | null = null;
/** Did this page already run under a controller? A `controllerchange` from
 *  none to one is the first install's claim, not an update. */
let hadController = false;
let reloadPage: () => void = () => window.location.reload();
let registration: ServiceWorkerRegistration | null = null;
let started = false;

function clearHandoverTimer(): void {
	if (handoverTimer !== null) clearTimeout(handoverTimer);
	handoverTimer = null;
}

/** The reload for an update: the user asked for it, so the terminal prompt
 *  stands aside; the unsaved-edits prompt still applies. */
function reloadForUpdate(): void {
	withTerminalGuardSuspended(() => reloadPage());
}

/** Record `worker` as waiting once it has installed — but only as an UPDATE:
 *  with no controller this is the first install, which needs no reload. */
function trackInstalling(worker: ServiceWorker | null, container: ServiceWorkerContainer): void {
	if (!worker) return;
	const check = () => {
		if (worker.state === 'installed' && container.controller) {
			usePwaStore.getState().setWaiting(worker);
		}
	};
	worker.addEventListener('statechange', check);
	check();
}

/**
 * Register `/sw.js` when this page is allowed to (see the module docs).
 * Returns the registration, or `null` when blocked or when registering
 * failed. Never throws; idempotent.
 */
export async function registerServiceWorker(
	env: RegistrationEnv = currentRegistrationEnv()
): Promise<ServiceWorkerRegistration | null> {
	if (registrationBlocker(env)) return null;
	if (started) return registration;
	started = true;

	const container = navigator.serviceWorker;

	// The install prompt is captured whatever happens to the worker, so the
	// install hint can offer a real button where the browser allows one.
	window.addEventListener('beforeinstallprompt', (e) => {
		e.preventDefault();
		usePwaStore.getState().setInstallPrompt(e as InstallPromptEvent);
	});
	window.addEventListener('appinstalled', () => usePwaStore.getState().setInstallPrompt(null));

	hadController = container.controller != null;
	container.addEventListener('controllerchange', () => {
		const hadBefore = hadController;
		hadController = container.controller != null;
		if (reloadRequested) {
			reloadRequested = false;
			clearHandoverTimer();
			reloadForUpdate();
			return;
		}
		// Not ours. The first install's claim (no controller before) needs
		// nothing; anything else means a newer worker took over while this
		// tab still runs the old code. Raise the banner, never auto-reload.
		const { waiting, setWaiting } = usePwaStore.getState();
		if (hadBefore && !waiting && container.controller) setWaiting(container.controller);
	});

	try {
		registration = await container.register(SW_URL, { scope: '/', updateViaCache: 'none' });
	} catch (err) {
		console.warn('[pwa] service worker registration failed', err);
		started = false;
		return null;
	}

	const reg = registration;
	if (reg.waiting && container.controller) usePwaStore.getState().setWaiting(reg.waiting);
	trackInstalling(reg.installing, container);
	reg.addEventListener('updatefound', () => trackInstalling(reg.installing, container));

	const check = () => {
		void reg.update().catch(() => {});
	};
	document.addEventListener('visibilitychange', () => {
		if (document.visibilityState === 'visible') check();
	});
	setInterval(check, UPDATE_INTERVAL_MS);

	return reg;
}

/**
 * "Reload to update": hand control to the waiting worker, then reload once it
 * has taken over. With no waiting worker, or one that is not waiting any more
 * (already active, or gone redundant: `SKIP_WAITING` would do nothing and no
 * `controllerchange` would follow), this is a plain reload.
 */
export function applyUpdate(): void {
	const waiting = usePwaStore.getState().waiting;
	const controller =
		typeof navigator === 'undefined' ? null : (navigator.serviceWorker?.controller ?? null);
	if (!waiting || !controller || waiting.state !== 'installed' || waiting === controller) {
		reloadForUpdate();
		return;
	}
	reloadRequested = true;
	waiting.postMessage(SKIP_WAITING_MESSAGE);
	// If the takeover never signals, reload anyway rather than sit on a banner
	// that does nothing.
	clearHandoverTimer();
	handoverTimer = setTimeout(() => {
		handoverTimer = null;
		if (!reloadRequested) return;
		reloadRequested = false;
		reloadForUpdate();
	}, HANDOVER_TIMEOUT_MS);
}

/** Resolves once `worker` has finished installing (or failed), or on timeout. */
function installed(worker: ServiceWorker, timeoutMs: number): Promise<void> {
	return new Promise((resolve) => {
		const done = () => {
			if (worker.state !== 'installing') {
				clearTimeout(timer);
				worker.removeEventListener('statechange', done);
				resolve();
			}
		};
		const timer = setTimeout(() => {
			worker.removeEventListener('statechange', done);
			resolve();
		}, timeoutMs);
		worker.addEventListener('statechange', done);
		done();
	});
}

const RECOVERY_KEY = 'ikenga_pwa_recovery_at';
const RECOVERY_WINDOW_MS = 60_000;

/**
 * Boot failed to load a module. If that is because this tab runs an old cached
 * shell whose chunks the server no longer has, a newer worker is (or can now
 * be) waiting: activate it and reload. Returns true when a reload is under
 * way. At most once a minute per tab, so a genuinely broken build can't loop.
 */
export async function recoverStaleShell(): Promise<boolean> {
	if (registrationBlocker(currentRegistrationEnv())) return false;
	try {
		const last = Number(sessionStorage.getItem(RECOVERY_KEY) ?? 0);
		if (Date.now() - last < RECOVERY_WINDOW_MS) return false;
	} catch {
		// No storage: still try once; the reload itself is the only side effect.
	}
	try {
		const reg = await navigator.serviceWorker.getRegistration('/');
		if (!reg) return false;
		await reg.update();
		if (!reg.waiting && reg.installing) await installed(reg.installing, 10_000);
		const waiting = reg.waiting ?? null;
		if (!waiting || !navigator.serviceWorker.controller) return false;
		try {
			sessionStorage.setItem(RECOVERY_KEY, String(Date.now()));
		} catch {
			// See above.
		}
		usePwaStore.getState().setWaiting(waiting);
		applyUpdate();
		return true;
	} catch {
		return false;
	}
}

/** Test seam. */
export function _resetRegisterForTests(reload?: () => void): void {
	reloadPage = reload ?? (() => window.location.reload());
	reloadRequested = false;
	clearHandoverTimer();
	hadController = false;
	_resetUnloadSuspendForTests();
	registration = null;
	started = false;
}
