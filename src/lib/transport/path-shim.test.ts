import { beforeEach, describe, expect, it, vi } from 'vitest';

const invoke = vi.fn();

vi.mock('./index', () => ({
	isTauri: () => false,
	getTransport: () => ({ invoke }),
}));

describe('path-shim against a reachable daemon', () => {
	beforeEach(() => {
		invoke.mockReset();
		vi.resetModules();
	});

	it('resolves homeDir over fs_home', async () => {
		invoke.mockResolvedValue('/home/operator');
		const { homeDir } = await import('./path-shim');
		await expect(homeDir()).resolves.toBe('/home/operator');
		expect(invoke).toHaveBeenCalledWith('fs_home');
	});

	it('builds appDataDir from the resolved home, not a hardcoded path', async () => {
		invoke.mockResolvedValue('/var/lib/ikenga');
		const { appDataDir } = await import('./path-shim');
		// Hardcoding `~/.local/share/...` produced a literal `~` that reaches
		// `fs_read` unexpanded and fails as a missing file, and was wrong
		// outright for a daemon whose HOME is not a Linux user home.
		await expect(appDataDir()).resolves.toBe('/var/lib/ikenga/.local/share/app.ikenga');
	});

	it('caches the home so each path build is not another round trip', async () => {
		invoke.mockResolvedValue('/home/operator');
		const { homeDir } = await import('./path-shim');
		await homeDir();
		await homeDir();
		expect(invoke).toHaveBeenCalledTimes(1);
	});

	it('falls back to ~ and warns when fs_home is unavailable', async () => {
		invoke.mockRejectedValue(new Error('unknown command'));
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
		const { homeDir } = await import('./path-shim');
		await expect(homeDir()).resolves.toBe('~');
		// Silence here propagates a bogus `~` into every derived path and
		// resurfaces later as an unrelated-looking file error.
		expect(warn).toHaveBeenCalled();
		warn.mockRestore();
	});
});
