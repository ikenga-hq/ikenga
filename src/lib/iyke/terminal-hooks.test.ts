import { beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	remote: true,
	info: vi.fn(),
	snaps: vi.fn(),
	decide: vi.fn(),
	iykeFetch: vi.fn(),
}));

vi.mock('@/lib/transport', () => ({ isRemoteWebSession: () => h.remote }));
vi.mock('@/lib/tauri-cmd', () => ({
	termHooksInfo: h.info,
	termHooksStatuslineSnapshot: h.snaps,
	termHooksDecide: h.decide,
}));
vi.mock('./client', () => ({ iykeFetch: h.iykeFetch }));

import {
	__resetTerminalHooksForTests,
	claudeHooksUnavailableReason,
	decideHookGateRemote,
	fetchStatuslineSnapshots,
	primeRemoteHooksInfo,
} from './terminal-hooks';

beforeEach(() => {
	h.remote = true;
	h.info.mockReset();
	h.snaps.mockReset();
	h.decide.mockReset();
	h.iykeFetch.mockReset();
	__resetTerminalHooksForTests();
});

describe('statusline snapshots', () => {
	it('come from the daemon arm in a browser, never the bridge', async () => {
		h.snaps.mockResolvedValue({ t1: { session_id: 's' } });
		expect(await fetchStatuslineSnapshots()).toEqual({ t1: { session_id: 's' } });
		expect(h.iykeFetch).not.toHaveBeenCalled();
	});

	it('are null when the daemon cannot say, rather than throwing into the HUD', async () => {
		h.snaps.mockRejectedValue(new Error('boom'));
		expect(await fetchStatuslineSnapshots()).toBeNull();
	});

	it('publishes reason if snapshot rejected with honest reason and info was not set', async () => {
		h.snaps.mockRejectedValue(new Error('Not available on this server: no data folder'));
		expect(await fetchStatuslineSnapshots()).toBeNull();
		expect(claudeHooksUnavailableReason()).toBe('Not available on this server: no data folder');
	});

	it('come from the bridge on the desktop', async () => {
		h.remote = false;
		h.iykeFetch.mockResolvedValue({ ok: true, json: async () => ({ t2: {} }) });
		expect(await fetchStatuslineSnapshots()).toEqual({ t2: {} });
		expect(h.snaps).not.toHaveBeenCalled();
		h.iykeFetch.mockResolvedValue({ ok: false });
		expect(await fetchStatuslineSnapshots()).toBeNull();
	});
});

describe('the hook-settings prime', () => {
	it('asks the daemon once and keeps its answer, a refusal included', async () => {
		h.info.mockResolvedValue({ settingsDir: null, reason: 'Not available on this server: x' });
		const a = await primeRemoteHooksInfo();
		await primeRemoteHooksInfo();
		expect(h.info).toHaveBeenCalledTimes(1);
		expect(a.settingsDir).toBeNull();
		expect(claudeHooksUnavailableReason()).toBe('Not available on this server: x');
	});

	it('reads an available server as no reason at all', async () => {
		h.info.mockResolvedValue({ settingsDir: '/d/term-hooks', reason: null });
		expect((await primeRemoteHooksInfo()).settingsDir).toBe('/d/term-hooks');
		expect(claudeHooksUnavailableReason()).toBeNull();
	});

	it('treats a daemon that does not know the arm as unavailable, once', async () => {
		h.info.mockRejectedValue(new Error("Command 'term_hooks_info' not implemented"));
		const i = await primeRemoteHooksInfo();
		await primeRemoteHooksInfo();
		expect(i.settingsDir).toBeNull();
		expect(claudeHooksUnavailableReason()).toBe('Not available on this server');
		expect(h.info).toHaveBeenCalledTimes(1);
	});
});

describe('answering a held gate in a browser', () => {
	it('reports whether the gate was still waiting', async () => {
		h.decide.mockResolvedValue({ recorded: true, gated: true });
		expect(await decideHookGateRemote('perm-1', 'approved')).toBe(true);
		expect(h.decide).toHaveBeenCalledWith('perm-1', 'approved');
		h.decide.mockResolvedValue({ recorded: true, gated: false });
		expect(await decideHookGateRemote('perm-1', 'denied')).toBe(false);
	});
});
