import { beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ remote: true, prime: vi.fn(), endpoint: vi.fn() }));

vi.mock('@/lib/transport', () => ({ isRemoteWebSession: () => h.remote }));
vi.mock('@/lib/iyke/client', () => ({ getEndpoint: h.endpoint }));
vi.mock('@/lib/iyke/terminal-hooks', () => ({ primeRemoteHooksInfo: h.prime }));

import {
	__setClaudeSettingsPathForTests,
	getClaudeSettingsPathSync,
	loadClaudeSettingsPath,
} from './claude-settings';

beforeEach(() => {
	h.remote = true;
	h.prime.mockReset();
	h.endpoint.mockReset();
	__setClaudeSettingsPathForTests(null);
});

describe('claude settings path in a browser session', () => {
	it("is the daemon's directory, never the iyke bridge's", async () => {
		h.prime.mockResolvedValue({ settingsDir: '/srv/data/term-hooks', reason: null });
		expect(await loadClaudeSettingsPath()).toBe('/srv/data/term-hooks');
		expect(getClaudeSettingsPathSync('t1')).toBe('/srv/data/term-hooks/claude-hooks-t1.json');
		expect(h.endpoint).not.toHaveBeenCalled();
	});

	it('is null, and final, when the daemon cannot take hooks: no retry on every launch', async () => {
		h.prime.mockResolvedValue({ settingsDir: null, reason: 'Not available on this server' });
		expect(await loadClaudeSettingsPath()).toBeNull();
		expect(getClaudeSettingsPathSync('t1')).toBeNull();
		expect(await loadClaudeSettingsPath()).toBeNull();
		expect(h.prime).toHaveBeenCalledTimes(1);
		expect(h.endpoint).not.toHaveBeenCalled();
	});
});

describe('claude settings path on the desktop', () => {
	it('still asks the iyke bridge and still retries a failed prime', async () => {
		h.remote = false;
		h.endpoint.mockRejectedValueOnce(new Error('iyke runtime not initialized'));
		expect(await loadClaudeSettingsPath()).toBeNull();
		h.endpoint.mockResolvedValueOnce({ app_local_data_dir: '/home/u/.local/share/app.ikenga' });
		expect(await loadClaudeSettingsPath()).toBe('/home/u/.local/share/app.ikenga');
		expect(h.prime).not.toHaveBeenCalled();
	});
});
