// A browser session connected to ikenga-server must persist settings.
//
// `enqueueSettingsWrite` used to return early unless a Tauri runtime was present, so in a
// browser every onboarding / theme / pin write was dropped. Reads still went over RPC, so
// each boot loaded the server's untouched "first run" state over the browser's copy and sent
// the user back to onboarding.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const settingsSet = vi.fn(async () => undefined);
const settingsGetAll = vi.fn(async () => ({}));
const settingsWriteField = vi.fn(async () => null);

vi.mock('@/lib/tauri-cmd', async (importOriginal) => {
	const actual = await importOriginal<typeof import('@/lib/tauri-cmd')>();
	return {
		...actual,
		settingsSet: (...a: unknown[]) => (settingsSet as (...x: unknown[]) => unknown)(...a),
		settingsGetAll: (...a: unknown[]) => (settingsGetAll as (...x: unknown[]) => unknown)(...a),
		settingsWriteField: (...a: unknown[]) =>
			(settingsWriteField as (...x: unknown[]) => unknown)(...a),
	};
});

const TOKEN_KEY = 'ikenga_auth_token';

async function freshStore() {
	vi.resetModules();
	const mod = await import('./shell-store');
	return mod.useShellStore;
}

describe('settings writes outside Tauri', () => {
	beforeEach(() => {
		settingsSet.mockClear();
		settingsGetAll.mockClear();
		settingsWriteField.mockClear();
		sessionStorage.clear();
		localStorage.clear();
		delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
		window.history.replaceState(null, '', '/');
	});
	afterEach(() => {
		delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
	});

	it('persists onboarding progress for a browser tab that holds a daemon token', async () => {
		sessionStorage.setItem(TOKEN_KEY, 'tok');
		const store = await freshStore();
		store.getState().setOnboardingActiveIndex(2);
		await vi.waitFor(() => expect(settingsSet).toHaveBeenCalled());
		const keys = settingsSet.mock.calls.map((c) => (c as unknown[])[0]);
		expect(keys).toContain('onboarding.state');
		const payload = settingsSet.mock.calls.find(
			(c) => (c as unknown[])[0] === 'onboarding.state'
		) as unknown[];
		expect(JSON.parse(String(payload[1])).activeIndex).toBe(2);
	});

	it('still writes nothing for a tab with no backend at all (no Tauri, no token)', async () => {
		const store = await freshStore();
		store.getState().setOnboardingActiveIndex(2);
		await new Promise((r) => setTimeout(r, 30));
		expect(settingsSet).not.toHaveBeenCalled();
	});

	it('still writes under Tauri', async () => {
		(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
		const store = await freshStore();
		store.getState().setOnboardingActiveIndex(3);
		await vi.waitFor(() => expect(settingsSet).toHaveBeenCalled());
	});
});

describe('settings while a share is open', () => {
	// Every RPC in a share goes to the Owner's daemon. Reading its settings put
	// the viewer into an un-onboarded Owner's first-run state; writing them would
	// change the Owner's own theme and onboarding.
	beforeEach(() => {
		settingsSet.mockClear();
		settingsGetAll.mockClear();
		settingsWriteField.mockClear();
		sessionStorage.clear();
		localStorage.clear();
		window.history.replaceState(null, '', '/');
	});

	async function openShare() {
		const transport = await import('@/lib/transport');
		transport.setShareMode({
			projectKey: '01a10ebe-6f8b-7050-b24b-50f70b956726/ruby-jobs',
			projectId: 'ruby-jobs',
			projectName: 'Ruby jobs',
			ownerUsername: 'ruby',
			role: 'operator',
			scope: 'project',
		});
	}

	it('neither reads nor writes the Owner settings', async () => {
		sessionStorage.setItem(TOKEN_KEY, 'tok');
		const store = await freshStore();
		const { useIkengaStore } = await import('@/lib/ikenga/theme-store');
		await openShare();
		await store.getState().hydrateSettingsFromRust();
		await useIkengaStore.getState().hydrateAppearanceFromRust();
		store.getState().setOnboardingActiveIndex(2);
		useIkengaStore.getState().setTheme('B');
		await new Promise((r) => setTimeout(r, 30));
		expect(settingsGetAll).not.toHaveBeenCalled();
		expect(settingsSet).not.toHaveBeenCalled();
		expect(settingsWriteField).not.toHaveBeenCalled();
	});

	it('writes again once the share is closed', async () => {
		sessionStorage.setItem(TOKEN_KEY, 'tok');
		const store = await freshStore();
		await openShare();
		const transport = await import('@/lib/transport');
		transport.setShareMode(null);
		store.getState().setOnboardingActiveIndex(2);
		await vi.waitFor(() => expect(settingsSet).toHaveBeenCalled());
	});
});
