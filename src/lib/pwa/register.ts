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

import { isTauri } from '@/lib/transport';
import { SKIP_WAITING_MESSAGE } from './sw-logic';
import { type InstallPromptEvent, usePwaStore } from './update-store';

export const SW_URL = '/sw.js';
const UPDATE_INTERVAL_MS = 60 * 60 * 1000;

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
let reloadPage: () => void = () => window.location.reload();
let registration: ServiceWorkerRegistration | null = null;
let started = false;

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

	container.addEventListener('controllerchange', () => {
		if (!reloadRequested) return;
		reloadRequested = false;
		reloadPage();
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
 * has taken over. With no waiting worker this is a plain reload.
 */
export function applyUpdate(): void {
	const waiting = usePwaStore.getState().waiting;
	if (!waiting || typeof navigator === 'undefined' || !navigator.serviceWorker?.controller) {
		reloadPage();
		return;
	}
	reloadRequested = true;
	waiting.postMessage(SKIP_WAITING_MESSAGE);
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
	registration = null;
	started = false;
}
