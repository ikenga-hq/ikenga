// Gap audit rank 29 — ⌘⇧L "Lock now" (`people.lock-now`). App lock guards
// the desktop app; the headless daemon does not serve `app_lock_lock`, so in a
// browser the key failed silently. It is now a no-op outside Tauri and still
// locks on the desktop.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	tauri: false,
	appLockLock: vi.fn(async () => ({ locked: true, secretSet: true })),
	setStatus: vi.fn(),
	status: { locked: false, secretSet: true } as { locked: boolean; secretSet: boolean } | null,
}));

vi.mock('@/lib/tauri-cmd', () => ({
	isTauri: () => h.tauri,
	appLockLock: h.appLockLock,
}));
vi.mock('@/shell/people/app-lock-store', () => ({
	useAppLockStore: { getState: () => ({ status: h.status, setStatus: h.setStatus }) },
}));

import { FRAME_COMMANDS } from './commands';

/** The handler is fire-and-forget; let its lazy imports and awaits settle. */
async function settle() {
	for (let i = 0; i < 10; i++) await new Promise((r) => setTimeout(r, 0));
}

beforeEach(() => {
	h.appLockLock.mockClear();
	h.setStatus.mockClear();
});
afterEach(() => {
	h.tauri = false;
});

describe('people.lock-now (gap rank 29)', () => {
	it('does not call app_lock_lock outside the desktop app', async () => {
		h.tauri = false;
		FRAME_COMMANDS['people.lock-now']({ command: 'people.lock-now', source: 'key' });
		await settle();
		expect(h.appLockLock).not.toHaveBeenCalled();
		expect(h.setStatus).not.toHaveBeenCalled();
	});

	it('locks on the desktop', async () => {
		h.tauri = true;
		FRAME_COMMANDS['people.lock-now']({ command: 'people.lock-now', source: 'key' });
		await vi.waitFor(() => expect(h.appLockLock).toHaveBeenCalledTimes(1));
		await vi.waitFor(() =>
			expect(h.setStatus).toHaveBeenCalledWith({ locked: true, secretSet: true })
		);
	});
});
