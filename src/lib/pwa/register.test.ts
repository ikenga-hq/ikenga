// plans/pwa S1: the service worker registers only for the browser-served
// production app in a secure context — NEVER under Tauri — and the update
// hand-over reloads exactly once, only when the user asked for it.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const transport = vi.hoisted(() => ({ tauri: false }));
vi.mock('@/lib/transport', async (importOriginal) => ({
	...(await importOriginal<typeof import('@/lib/transport')>()),
	isTauri: () => transport.tauri,
}));

// The real terminal store opens SQLite on import; the guard is driven through
// injected deps here, so a stub is all it needs.
vi.mock('@/terminal/session-store', () => ({
	useTerminalStore: { getState: () => ({ tabs: [] }), subscribe: () => () => {} },
}));

import { useEditingStore } from '@/lib/editing/editing-store';
import { installUnloadGuard } from '@/lib/window/unload-guard';
import {
	_resetRegisterForTests,
	applyUpdate,
	HANDOVER_TIMEOUT_MS,
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

/** An update has installed and is waiting, as the banner sees it. */
async function withWaitingUpdate(): Promise<FakeWorker> {
	container.controller = {};
	await registerServiceWorker(OK_ENV);
	const next = new FakeWorker();
	container.reg.installing = next;
	container.reg.dispatchEvent(new Event('updatefound'));
	next.setState('installed');
	expect(usePwaStore.getState().waiting).toBe(next);
	return next;
}

describe('applyUpdate when the stored worker has moved on', () => {
	it('reloads straight away when the stored worker already activated (SKIP_WAITING would be a no-op)', async () => {
		const next = await withWaitingUpdate();
		next.setState('activated');
		applyUpdate();
		expect(reload).toHaveBeenCalledTimes(1);
		expect(next.postMessage).not.toHaveBeenCalled();
	});

	it('reloads straight away when the stored worker is still activating', async () => {
		const next = await withWaitingUpdate();
		next.setState('activating');
		applyUpdate();
		expect(reload).toHaveBeenCalledTimes(1);
		expect(next.postMessage).not.toHaveBeenCalled();
	});

	it('reloads straight away when the stored worker went redundant', async () => {
		const next = await withWaitingUpdate();
		next.setState('redundant');
		applyUpdate();
		expect(reload).toHaveBeenCalledTimes(1);
		expect(next.postMessage).not.toHaveBeenCalled();
	});

	it('reloads straight away when the stored worker already is the controller', async () => {
		const next = await withWaitingUpdate(); // state stays "installed"
		container.controller = next;
		applyUpdate();
		expect(reload).toHaveBeenCalledTimes(1);
		expect(next.postMessage).not.toHaveBeenCalled();
	});

	it('does not leave a reload armed behind an immediate reload', async () => {
		const next = await withWaitingUpdate();
		next.setState('activated');
		applyUpdate();
		container.dispatchEvent(new Event('controllerchange'));
		vi.advanceTimersByTime(HANDOVER_TIMEOUT_MS * 2);
		expect(reload).toHaveBeenCalledTimes(1);
	});
});

describe('a controllerchange this tab did not ask for', () => {
	it('raises the banner and does not reload', async () => {
		container.controller = {};
		await registerServiceWorker(OK_ENV);
		expect(usePwaStore.getState().waiting).toBeNull();

		// Another tab applied the update: the controller is now the new worker.
		const taken = new FakeWorker();
		taken.state = 'activated';
		container.controller = taken;
		container.dispatchEvent(new Event('controllerchange'));

		expect(reload).not.toHaveBeenCalled();
		expect(usePwaStore.getState().waiting).toBe(taken);
		vi.advanceTimersByTime(HANDOVER_TIMEOUT_MS * 2);
		expect(reload).not.toHaveBeenCalled();

		// The banner's button then reloads (the stored worker is the controller).
		applyUpdate();
		expect(reload).toHaveBeenCalledTimes(1);
		expect(taken.postMessage).not.toHaveBeenCalled();
	});

	it('keeps a banner that is already up', async () => {
		const next = await withWaitingUpdate();
		const other = new FakeWorker();
		other.state = 'activated';
		container.controller = other;
		container.dispatchEvent(new Event('controllerchange'));
		expect(usePwaStore.getState().waiting).toBe(next);
		expect(reload).not.toHaveBeenCalled();
	});

	it('never reloads or raises the banner on the first install, even after it', async () => {
		container.reg.installing = new FakeWorker();
		await registerServiceWorker(OK_ENV);
		container.reg.installing.setState('installed');
		container.controller = {}; // clients.claim()
		container.dispatchEvent(new Event('controllerchange'));
		expect(reload).not.toHaveBeenCalled();
		expect(usePwaStore.getState().waiting).toBeNull();
		vi.advanceTimersByTime(HANDOVER_TIMEOUT_MS * 2);
		expect(reload).not.toHaveBeenCalled();
	});
});

describe('the hand-over timeout fallback', () => {
	it('reloads anyway when SKIP_WAITING is never followed by a controllerchange', async () => {
		const next = await withWaitingUpdate();
		applyUpdate();
		expect(next.postMessage).toHaveBeenCalledWith(SKIP_WAITING_MESSAGE);
		vi.advanceTimersByTime(HANDOVER_TIMEOUT_MS - 1);
		expect(reload).not.toHaveBeenCalled();
		vi.advanceTimersByTime(1);
		expect(reload).toHaveBeenCalledTimes(1);
	});

	it('a controllerchange in time cancels the fallback: one reload, not two', async () => {
		await withWaitingUpdate();
		applyUpdate();
		container.dispatchEvent(new Event('controllerchange'));
		expect(reload).toHaveBeenCalledTimes(1);
		vi.advanceTimersByTime(HANDOVER_TIMEOUT_MS * 2);
		expect(reload).toHaveBeenCalledTimes(1);
	});
});

describe('the update reload and the unload guards', () => {
	let teardownGuard: () => void;
	const beforeUnload = () => {
		const e = new Event('beforeunload', { cancelable: true });
		window.dispatchEvent(e);
		return e.defaultPrevented;
	};

	let prevented: boolean | null = null;

	beforeEach(() => {
		prevented = null;
		// A browser session with one terminal tab open: the terminal guard is armed.
		teardownGuard = installUnloadGuard({
			isBrowser: () => true,
			terminalCount: () => 1,
			subscribe: () => () => {},
		});
		useEditingStore.setState({ sessions: {} });
		// What the real reload does: the browser dispatches beforeunload.
		reload.mockImplementation(() => {
			prevented = beforeUnload();
		});
	});

	afterEach(() => {
		teardownGuard();
		useEditingStore.setState({ sessions: {} });
	});

	it('control: a plain tab close with a terminal open IS prompted', () => {
		expect(beforeUnload()).toBe(true);
	});

	it('an update reload is not blocked by the terminal prompt', async () => {
		const next = await withWaitingUpdate();
		next.setState('activated');
		applyUpdate();
		expect(reload).toHaveBeenCalledTimes(1);
		expect(prevented).toBe(false);
	});

	it('...also on the controllerchange path', async () => {
		await withWaitingUpdate();
		applyUpdate();
		container.dispatchEvent(new Event('controllerchange'));
		expect(reload).toHaveBeenCalledTimes(1);
		expect(prevented).toBe(false);
	});

	it('the terminal prompt is back for a later accidental close', async () => {
		const next = await withWaitingUpdate();
		next.setState('activated');
		applyUpdate();
		vi.advanceTimersByTime(2000);
		expect(beforeUnload()).toBe(true);
	});

	it('unsaved edits still stop the update reload', async () => {
		useEditingStore.getState().upsert('k', {
			path: '/a.md',
			paneId: 'p1',
			mounted: true,
			editing: true,
			dirty: true,
		});
		const next = await withWaitingUpdate();
		next.setState('activated');
		applyUpdate();
		expect(reload).toHaveBeenCalledTimes(1);
		expect(prevented).toBe(true);
	});
});
