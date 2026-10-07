// plans/pwa S1: the service worker registers only for the browser-served
// production app in a secure context — NEVER under Tauri — and the update
// hand-over reloads exactly once, only when the user asked for it.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const transport = vi.hoisted(() => ({ tauri: false }));
vi.mock('@/lib/transport', () => ({ isTauri: () => transport.tauri }));

import {
	_resetRegisterForTests,
	applyUpdate,
	type RegistrationEnv,
	registerServiceWorker,
	registrationBlocker,
} from './register';
import { SKIP_WAITING_MESSAGE } from './sw-logic';
import { usePwaStore } from './update-store';

const OK_ENV: RegistrationEnv = {
	isTauri: false,
	prod: true,
	hasServiceWorker: true,
	secureContext: true,
};

class FakeWorker extends EventTarget {
	state: ServiceWorkerState = 'installing';
	postMessage = vi.fn();
	setState(s: ServiceWorkerState) {
		this.state = s;
		this.dispatchEvent(new Event('statechange'));
	}
}

class FakeRegistration extends EventTarget {
	installing: FakeWorker | null = null;
	waiting: FakeWorker | null = null;
	update = vi.fn(async () => {});
}

class FakeContainer extends EventTarget {
	controller: object | null = null;
	reg = new FakeRegistration();
	register = vi.fn(async () => this.reg);
}

let container: FakeContainer;
const reload = vi.fn();

beforeEach(() => {
	vi.useFakeTimers();
	_resetRegisterForTests(reload);
	usePwaStore.setState({ waiting: null, installPrompt: null });
	transport.tauri = false;
	container = new FakeContainer();
	Object.defineProperty(navigator, 'serviceWorker', { configurable: true, value: container });
	reload.mockReset();
});

afterEach(() => {
	Reflect.deleteProperty(navigator, 'serviceWorker');
	vi.useRealTimers();
});

describe('registrationBlocker', () => {
	it('blocks the desktop app first, whatever else holds', () => {
		expect(registrationBlocker({ ...OK_ENV, isTauri: true })).toBe('tauri');
		expect(
			registrationBlocker({
				isTauri: true,
				prod: false,
				hasServiceWorker: false,
				secureContext: false,
			})
		).toBe('tauri');
	});

	it('blocks dev builds, browsers without workers and insecure origins', () => {
		expect(registrationBlocker({ ...OK_ENV, prod: false })).toBe('dev');
		expect(registrationBlocker({ ...OK_ENV, hasServiceWorker: false })).toBe('unsupported');
		expect(registrationBlocker({ ...OK_ENV, secureContext: false })).toBe('insecure');
		expect(registrationBlocker(OK_ENV)).toBeNull();
	});
});

describe('registerServiceWorker', () => {
	it('never registers under Tauri', async () => {
		transport.tauri = true;
		expect(await registerServiceWorker({ ...OK_ENV, isTauri: true })).toBeNull();
		expect(container.register).not.toHaveBeenCalled();
	});

	it('never registers on plain HTTP or in dev', async () => {
		expect(await registerServiceWorker({ ...OK_ENV, secureContext: false })).toBeNull();
		expect(await registerServiceWorker({ ...OK_ENV, prod: false })).toBeNull();
		expect(container.register).not.toHaveBeenCalled();
	});

	it('registers /sw.js at scope / with no HTTP caching of the script', async () => {
		await registerServiceWorker(OK_ENV);
		expect(container.register).toHaveBeenCalledWith('/sw.js', {
			scope: '/',
			updateViaCache: 'none',
		});
		// Idempotent.
		await registerServiceWorker(OK_ENV);
		expect(container.register).toHaveBeenCalledTimes(1);
	});

	it('the first install raises no update banner and does not reload', async () => {
		container.reg.installing = new FakeWorker();
		await registerServiceWorker(OK_ENV);
		container.reg.installing.setState('installed');
		expect(usePwaStore.getState().waiting).toBeNull();
		// clients.claim() on first activation fires controllerchange.
		container.controller = {};
		container.dispatchEvent(new Event('controllerchange'));
		expect(reload).not.toHaveBeenCalled();
	});

	it('an update waits for the user, then reloads exactly once', async () => {
		container.controller = {};
		await registerServiceWorker(OK_ENV);

		const next = new FakeWorker();
		container.reg.installing = next;
		container.reg.dispatchEvent(new Event('updatefound'));
		next.setState('installed');
		expect(usePwaStore.getState().waiting).toBe(next);
		expect(next.postMessage).not.toHaveBeenCalled();

		applyUpdate();
		expect(next.postMessage).toHaveBeenCalledWith(SKIP_WAITING_MESSAGE);
		expect(reload).not.toHaveBeenCalled();

		container.dispatchEvent(new Event('controllerchange'));
		container.dispatchEvent(new Event('controllerchange'));
		expect(reload).toHaveBeenCalledTimes(1);
	});

	it('picks up a worker already waiting from an earlier visit', async () => {
		container.controller = {};
		container.reg.waiting = new FakeWorker();
		await registerServiceWorker(OK_ENV);
		expect(usePwaStore.getState().waiting).toBe(container.reg.waiting);
	});

	it('survives a failed registration', async () => {
		container.register.mockRejectedValueOnce(new Error('SecurityError'));
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
		expect(await registerServiceWorker(OK_ENV)).toBeNull();
		warn.mockRestore();
	});
});
