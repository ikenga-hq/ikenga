import { describe, expect, it, vi } from 'vitest';

import { type PkgInstallResult, summariseBatch, triggerPkgInstalls } from './install-queue';

function makeKernelStatus(installedIds: string[]) {
	return vi.fn(async () => ({
		installed: installedIds.map((id) => ({
			id,
			version: '0.0.0',
			ikenga_api: '1',
			install_path: `/tmp/${id}`,
			enabled: true,
			installed_at: 0,
			compatible: true,
			source: { kind: 'builtin' as const },
		})),
		registries: {},
		api_version: 1,
	}));
}

describe('triggerPkgInstalls', () => {
	it('installs every selected pkg with a resolvable path', async () => {
		const install = vi.fn(async () => undefined);
		const results = await triggerPkgInstalls({
			selectedPkgIds: ['com.ikenga.tasks'],
			catalogResolver: () => ({ installPath: '/tmp/tasks' }),
			kernelStatus: makeKernelStatus([]),
			install,
		});
		expect(install).toHaveBeenCalledTimes(1);
		expect(install).toHaveBeenCalledWith('/tmp/tasks');
		expect(results).toEqual([
			{
				pkgId: 'com.ikenga.tasks',
				display: 'Tasks',
				ok: true,
				skipped: false,
			},
		]);
	});

	it('skips pkgs that are already installed', async () => {
		const install = vi.fn(async () => undefined);
		const results = await triggerPkgInstalls({
			selectedPkgIds: ['com.ikenga.tasks'],
			catalogResolver: () => ({ installPath: '/tmp/tasks' }),
			kernelStatus: makeKernelStatus(['com.ikenga.tasks']),
			install,
		});
		expect(install).not.toHaveBeenCalled();
		expect(results[0]?.skipped).toBe(true);
		expect(results[0]?.ok).toBe(true);
	});

	it('marks pkgs with no resolvable source as skipped (no-source)', async () => {
		const install = vi.fn();
		const results = await triggerPkgInstalls({
			selectedPkgIds: ['com.ikenga.unknown'],
			catalogResolver: () => null,
			kernelStatus: makeKernelStatus([]),
			install,
		});
		expect(install).not.toHaveBeenCalled();
		expect(results[0]?.skipped).toBe(true);
		expect(results[0]?.ok).toBe(false);
	});

	it('records per-pkg errors when install throws', async () => {
		const install = vi.fn(async () => {
			throw new Error('boom');
		});
		const results = await triggerPkgInstalls({
			selectedPkgIds: ['com.ikenga.tasks'],
			catalogResolver: () => ({ installPath: '/tmp/tasks' }),
			kernelStatus: makeKernelStatus([]),
			install,
		});
		expect(results[0]?.ok).toBe(false);
		expect(results[0]?.skipped).toBe(false);
		expect(results[0]?.error).toBe('boom');
	});

	it('survives a kernel-status read failure', async () => {
		const install = vi.fn(async () => undefined);
		const results = await triggerPkgInstalls({
			selectedPkgIds: ['com.ikenga.tasks'],
			catalogResolver: () => ({ installPath: '/tmp/tasks' }),
			kernelStatus: vi.fn(async () => {
				throw new Error('db locked');
			}),
			install,
		});
		expect(results[0]?.ok).toBe(true);
		expect(install).toHaveBeenCalledOnce();
	});
});

describe('summariseBatch', () => {
	function r(overrides: Partial<PkgInstallResult>): PkgInstallResult {
		return {
			pkgId: 'x',
			display: 'X',
			ok: true,
			skipped: false,
			...overrides,
		};
	}

	it('counts installed / alreadyPresent / failed / noSource buckets', () => {
		const out = summariseBatch([
			r({ pkgId: 'a', ok: true, skipped: false }),
			r({ pkgId: 'b', ok: true, skipped: true }),
			r({ pkgId: 'c', ok: false, skipped: false, error: 'x' }),
			r({ pkgId: 'd', ok: false, skipped: true, error: 'no source' }),
		]);
		expect(out).toEqual({ total: 4, installed: 1, alreadyPresent: 1, failed: 1, noSource: 1 });
	});

	it('handles empty input', () => {
		expect(summariseBatch([])).toEqual({
			total: 0,
			installed: 0,
			alreadyPresent: 0,
			failed: 0,
			noSource: 0,
		});
	});
});

describe('triggerPkgInstalls — unserved install (gap rank 3)', () => {
	it('records the honest reason, never a raw "not implemented" string, and never ok', async () => {
		const results = await triggerPkgInstalls({
			selectedPkgIds: ['com.ikenga.tasks'],
			catalogResolver: () => ({ installPath: '/tmp/tasks' }),
			kernelStatus: makeKernelStatus([]),
			install: vi.fn(async () => {
				throw new Error("Command 'pkg_install_from_path' not implemented in headless daemon");
			}),
		});
		expect(results[0]?.ok).toBe(false);
		expect(results[0]?.error).toBe('Not available on this server yet');
	});
});
