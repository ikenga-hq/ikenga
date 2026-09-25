// WP-40 — webview-side `update` producers. Written under DEC-50; not run
// until the 5b close (WP-47).

import { afterEach, describe, expect, it, vi } from 'vitest';

const cmd = vi.hoisted(() => ({
	isTauri: vi.fn(() => true),
	notificationsRecordUpdate: vi.fn(async () => null),
}));

vi.mock('@/lib/tauri-cmd', () => cmd);

import { recordPkgUpdatesAvailable, recordShellUpdateAvailable } from './record-update';

describe('update producers', () => {
	afterEach(() => {
		cmd.isTauri.mockReset();
		cmd.isTauri.mockReturnValue(true);
		cmd.notificationsRecordUpdate.mockReset();
		cmd.notificationsRecordUpdate.mockResolvedValue(null);
	});

	it('records a shell update by version', async () => {
		await recordShellUpdateAvailable('0.9.1');
		expect(cmd.notificationsRecordUpdate).toHaveBeenCalledWith({
			source: 'shell',
			version: '0.9.1',
		});
	});

	it('records one row per outdated pkg and skips rows without a latest', async () => {
		await recordPkgUpdatesAvailable([
			{ id: 'com.ikenga.tasks', name: 'Tasks', latest: '1.2.0' },
			{ id: 'com.ikenga.none', name: 'None', latest: null },
		]);
		expect(cmd.notificationsRecordUpdate).toHaveBeenCalledOnce();
		expect(cmd.notificationsRecordUpdate).toHaveBeenCalledWith({
			source: 'pkg',
			version: '1.2.0',
			pkgId: 'com.ikenga.tasks',
			pkgName: 'Tasks',
		});
	});

	it('is a no-op outside the desktop runtime or without a version', async () => {
		cmd.isTauri.mockReturnValue(false);
		await recordShellUpdateAvailable('0.9.1');
		await recordPkgUpdatesAvailable([{ id: 'p', latest: '1.0.0' }]);
		cmd.isTauri.mockReturnValue(true);
		await recordShellUpdateAvailable(null);
		expect(cmd.notificationsRecordUpdate).not.toHaveBeenCalled();
	});

	it('never rejects when the command fails', async () => {
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
		cmd.notificationsRecordUpdate.mockRejectedValue(new Error('boom'));
		await expect(recordShellUpdateAvailable('0.9.1')).resolves.toBeUndefined();
		await expect(
			recordPkgUpdatesAvailable([{ id: 'p', latest: '1.0.0' }]),
		).resolves.toBeUndefined();
		expect(warn).toHaveBeenCalled();
		warn.mockRestore();
	});
});
