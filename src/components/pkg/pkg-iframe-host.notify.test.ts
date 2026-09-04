// Verb-level tests for host.notify (WP-26) — the OS-notification bridge for
// pkgs.
//
// Covers the `permissions.notify` scope gate (mirrors `engine:invoke`'s
// `pkgDeclaresScope` shape), the required `title` argument, the OS
// permission round-trip (already-granted vs prompt-then-denied), and the
// per-pkg rate limit. `sendNotification` / `isNotificationPermissionGranted`
// / `requestNotificationPermission` are mocked at the `@/lib/transport/shims`
// boundary — this layer only proves the dispatcher's guards fire and that an
// authorized call threads title/body through.

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

import { pkgKernelStatus, pkgPreviewManifest } from '@/lib/tauri-cmd';
import {
	isNotificationPermissionGranted,
	requestNotificationPermission,
	sendNotification,
} from '@/lib/transport/shims';
import { dispatchHostCall } from './pkg-iframe-host';

const kernelStatus = vi.mocked(pkgKernelStatus);
const previewManifest = vi.mocked(pkgPreviewManifest);
const permGranted = vi.mocked(isNotificationPermissionGranted);
const permRequest = vi.mocked(requestNotificationPermission);
const notify = vi.mocked(sendNotification);

const PKG = 'com.ikenga.meetings';

// The dispatcher's notify rate limiter is per-pkg module state, not reset
// between tests — so each test gets its own synthetic pkg id to stay
// isolated from every other test's call count. Only the dedicated
// rate-limit test below deliberately reuses one id across several calls.
let pkgCounter = 0;
function freshPkgId(): string {
	pkgCounter += 1;
	return `${PKG}.${pkgCounter}`;
}

// `notify` present with `send` = the pkg declared the `notify:send` scope.
function withScope(id: string, hasSend: boolean) {
	kernelStatus.mockResolvedValue({
		installed: [{ id, install_path: `/pkgs/${id}` }],
		registries: {},
		api_version: 1,
	} as never);
	previewManifest.mockResolvedValue({
		id,
		name: id,
		version: '1.0.0',
		ikenga_api: '1',
		capabilities: {},
		permissions: hasSend ? { notify: ['send'] } : {},
	} as never);
}

beforeEach(() => {
	vi.clearAllMocks();
	permGranted.mockResolvedValue(true);
	permRequest.mockResolvedValue('granted' as never);
	notify.mockResolvedValue(undefined as never);
});

describe('host.notify (pkg-iframe OS notification verb, WP-26)', () => {
	it('sends a notification when the notify:send scope is declared', async () => {
		const id = freshPkgId();
		withScope(id, true);

		const res = await dispatchHostCall(id, 'host.notify', {
			title: 'Meeting starting',
			body: 'Weekly sync begins in 2 minutes',
		});

		expect(notify).toHaveBeenCalledWith({
			title: 'Meeting starting',
			body: 'Weekly sync begins in 2 minutes',
		});
		expect(res.isError).toBeUndefined();
		expect(res.structuredContent).toEqual({ ok: true });
	});

	it('denies the call without the notify:send scope (scope-denied, no send)', async () => {
		const id = freshPkgId();
		withScope(id, false);

		const res = await dispatchHostCall(id, 'host.notify', { title: 'Nope' });

		expect(res.isError).toBe(true);
		expect(res.structuredContent).toEqual({ ok: false, reason: 'scope-denied' });
		expect(notify).not.toHaveBeenCalled();
	});

	it('rejects a missing title before checking scope', async () => {
		const id = freshPkgId();
		withScope(id, true);

		const res = await dispatchHostCall(id, 'host.notify', {});

		expect(res.isError).toBe(true);
		expect(notify).not.toHaveBeenCalled();
	});

	it('requests OS permission when not already granted, then sends', async () => {
		const id = freshPkgId();
		withScope(id, true);
		permGranted.mockResolvedValue(false);
		permRequest.mockResolvedValue('granted' as never);

		const res = await dispatchHostCall(id, 'host.notify', { title: 'Nudge' });

		expect(permRequest).toHaveBeenCalled();
		expect(notify).toHaveBeenCalledWith({ title: 'Nudge', body: undefined });
		expect(res.structuredContent).toEqual({ ok: true });
	});

	it('surfaces a denied OS permission as a non-ok envelope (no send)', async () => {
		const id = freshPkgId();
		withScope(id, true);
		permGranted.mockResolvedValue(false);
		permRequest.mockResolvedValue('denied' as never);

		const res = await dispatchHostCall(id, 'host.notify', { title: 'Nudge' });

		expect(res.isError).toBe(true);
		expect(res.structuredContent).toEqual({ ok: false, reason: 'permission-denied' });
		expect(notify).not.toHaveBeenCalled();
	});

	it('rate-limits a pkg sending more than 3 notifications within 60s', async () => {
		const id = freshPkgId();
		withScope(id, true);

		for (let i = 0; i < 3; i++) {
			const res = await dispatchHostCall(id, 'host.notify', { title: `Ping ${i}` });
			expect(res.structuredContent).toEqual({ ok: true });
		}
		const fourth = await dispatchHostCall(id, 'host.notify', { title: 'Ping 4' });

		expect(fourth.isError).toBe(true);
		expect(fourth.structuredContent).toEqual({ ok: false, reason: 'rate-limited' });
		expect(notify).toHaveBeenCalledTimes(3);
	});
});
