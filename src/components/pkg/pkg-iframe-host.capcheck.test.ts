// Capability checks are tri-state. A transient kernel / manifest read failure
// still refuses the call (fail-closed) but must not be reported to the pkg as
// "scope not declared": it carries the additive `reason: 'check-unavailable'`.
// A real denial keeps its exact existing message and envelope.

import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/tauri-cmd', () => ({
	dbQuery: vi.fn(),
	dbExec: vi.fn(),
	pkgKernelStatus: vi.fn(),
	pkgPreviewManifest: vi.fn(),
	pkgContentHtml: vi.fn(),
	pkgContentRevoke: vi.fn(),
	pkgMcpCall: vi.fn(),
	pkgSidecarCall: vi.fn(),
}));

vi.mock('@/lib/transport/shims', () => ({
	isNotificationPermissionGranted: vi.fn(),
	requestNotificationPermission: vi.fn(),
	sendNotification: vi.fn(),
}));

import { dbQuery, pkgKernelStatus, pkgPreviewManifest } from '@/lib/tauri-cmd';
import { sendNotification } from '@/lib/transport/shims';
import { dispatchHostCall } from './pkg-iframe-host';

const kernelStatus = vi.mocked(pkgKernelStatus);
const previewManifest = vi.mocked(pkgPreviewManifest);
const query = vi.mocked(dbQuery);

const PKG = 'com.ikenga.tasks';

function installed() {
	kernelStatus.mockResolvedValue({
		installed: [{ id: PKG, install_path: `/pkgs/${PKG}` }],
		registries: {},
		api_version: 1,
	} as never);
}

beforeEach(() => {
	vi.clearAllMocks();
});

describe('capability checks: denied vs unavailable', () => {
	it('a kernel status failure is reported as check-unavailable, not as a missing capability', async () => {
		kernelStatus.mockRejectedValue(new Error('kernel busy'));

		const res = await dispatchHostCall(PKG, 'host.dbQuery', { sql: 'SELECT * FROM tasks' });

		expect(res.isError).toBe(true);
		expect(res.structuredContent).toMatchObject({ ok: false, reason: 'check-unavailable' });
		const text = (res.content[0] as { text: string }).text;
		expect(text).toContain('kernel busy');
		expect(text).not.toMatch(/lacks/);
		expect(query).not.toHaveBeenCalled();
	});

	it('a manifest read failure on the sqlite.tables lookup is check-unavailable too', async () => {
		installed();
		previewManifest
			.mockResolvedValueOnce({ capabilities: { sqlite: {} }, permissions: {} } as never)
			.mockRejectedValueOnce(new Error('EBUSY manifest.json'));

		const res = await dispatchHostCall(PKG, 'host.dbQuery', { sql: 'SELECT * FROM tasks' });

		expect(res.structuredContent).toMatchObject({ reason: 'check-unavailable' });
		expect(query).not.toHaveBeenCalled();
	});

	it('a real denial keeps the existing message and envelope (no reason field)', async () => {
		installed();
		previewManifest.mockResolvedValue({ capabilities: {}, permissions: {} } as never);

		const res = await dispatchHostCall(PKG, 'host.dbQuery', { sql: 'SELECT * FROM tasks' });

		const msg = "host.dbQuery: pkg lacks the 'sqlite' capability";
		expect(res.structuredContent).toEqual({ ok: false, error: msg });
		expect((res.content[0] as { text: string }).text).toBe(msg);
	});

	it('host.notify: unavailable is not reported as scope-denied', async () => {
		kernelStatus.mockRejectedValue(new Error('ipc closed'));

		const res = await dispatchHostCall('com.ikenga.meetings.cap', 'host.notify', { title: 'hi' });

		expect(res.isError).toBe(true);
		expect(res.structuredContent).toMatchObject({ reason: 'check-unavailable' });
		expect(vi.mocked(sendNotification)).not.toHaveBeenCalled();
	});

	it('host.notify: a real denial still reports reason scope-denied', async () => {
		kernelStatus.mockResolvedValue({
			installed: [{ id: 'com.ikenga.meetings.cap2', install_path: '/pkgs/m' }],
			registries: {},
			api_version: 1,
		} as never);
		previewManifest.mockResolvedValue({ permissions: {} } as never);

		const res = await dispatchHostCall('com.ikenga.meetings.cap2', 'host.notify', { title: 'hi' });

		expect(res.structuredContent).toEqual({ ok: false, reason: 'scope-denied' });
	});
});
