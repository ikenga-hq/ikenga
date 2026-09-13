// Verb-level tests for the approve-gate write verbs (host.paActions.*, WP-18a).
//
// Covers the four thin wrappers over the tested `pa_actions_*` Rust commands:
// the `engine:invoke` scope gate (shared with host.sendToActiveSession), the
// draftId requirement, that an authorized call threads through to the right
// Rust command, and that a thrown Rust error surfaces as a non-ok envelope.
// The Rust `pa_actions_*` are mocked at the `@/lib/tauri-cmd` boundary — this
// layer only proves the dispatcher's guards fire and dispatch is wired.

import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/tauri-cmd', () => ({
	dbQuery: vi.fn(),
	dbExec: vi.fn(),
	paActionsCommit: vi.fn(),
	paActionsReject: vi.fn(),
	paActionsRetry: vi.fn(),
	paActionsUpdate: vi.fn(),
	pkgKernelStatus: vi.fn(),
	pkgPreviewManifest: vi.fn(),
	pkgContentHtml: vi.fn(),
	pkgContentRevoke: vi.fn(),
	pkgMcpCall: vi.fn(),
	pkgSidecarCall: vi.fn(),
	ptyWrite: vi.fn(),
}));

import {
	paActionsCommit,
	paActionsReject,
	paActionsRetry,
	paActionsUpdate,
	pkgKernelStatus,
	pkgPreviewManifest,
	ptyWrite,
} from '@/lib/tauri-cmd';
import { useTerminalStore } from '@/terminal/session-store';
import { dispatchHostCall } from './pkg-iframe-host';

const kernelStatus = vi.mocked(pkgKernelStatus);
const previewManifest = vi.mocked(pkgPreviewManifest);
const commit = vi.mocked(paActionsCommit);
const reject = vi.mocked(paActionsReject);
const retry = vi.mocked(paActionsRetry);
const update = vi.mocked(paActionsUpdate);
const mockPtyWrite = vi.mocked(ptyWrite);

const PKG = 'com.ikenga.outbound';
const DRAFT = 'draft-42';

// `engine` present with `invoke` = the pkg declared the `engine:invoke` scope.
function withScope(hasInvoke: boolean) {
	kernelStatus.mockResolvedValue({
		installed: [{ id: PKG, install_path: `/pkgs/${PKG}` }],
		registries: {},
		api_version: 1,
	} as never);
	previewManifest.mockResolvedValue({
		id: PKG,
		name: PKG,
		version: '1.0.0',
		ikenga_api: '1',
		capabilities: {},
		permissions: hasInvoke ? { engine: ['invoke'] } : {},
	} as never);
}

beforeEach(() => {
	vi.clearAllMocks();
});

describe('host.paActions.* (pkg-iframe approve-gate write verbs)', () => {
	it('commit dispatches to paActionsCommit when engine:invoke is declared', async () => {
		withScope(true);
		commit.mockResolvedValue(undefined as never);

		const res = await dispatchHostCall(PKG, 'host.paActions.commit', { draftId: DRAFT });

		expect(commit).toHaveBeenCalledWith(DRAFT);
		expect(res.isError).toBeUndefined();
		expect(res.structuredContent).toEqual({ ok: true });
	});

	it('reject dispatches to paActionsReject', async () => {
		withScope(true);
		reject.mockResolvedValue(undefined as never);

		const res = await dispatchHostCall(PKG, 'host.paActions.reject', { draftId: DRAFT });

		expect(reject).toHaveBeenCalledWith(DRAFT);
		expect(res.structuredContent).toEqual({ ok: true });
	});

	it('retry dispatches to paActionsRetry', async () => {
		withScope(true);
		retry.mockResolvedValue(undefined as never);

		const res = await dispatchHostCall(PKG, 'host.paActions.retry', { draftId: DRAFT });

		expect(retry).toHaveBeenCalledWith(DRAFT);
		expect(res.structuredContent).toEqual({ ok: true });
	});

	it('update threads a subject/body patch through to paActionsUpdate', async () => {
		withScope(true);
		update.mockResolvedValue(undefined as never);

		const res = await dispatchHostCall(PKG, 'host.paActions.update', {
			draftId: DRAFT,
			patch: { subject: 'New subject', body: 'New body', junk: 1 },
		});

		// Only subject/body are threaded; the extra field is dropped.
		expect(update).toHaveBeenCalledWith(DRAFT, { subject: 'New subject', body: 'New body' });
		expect(res.structuredContent).toEqual({ ok: true });
	});

	it('denies every verb without the engine:invoke scope (scope-denied, no dispatch)', async () => {
		withScope(false);

		for (const name of [
			'host.paActions.commit',
			'host.paActions.reject',
			'host.paActions.retry',
			'host.paActions.update',
		]) {
			const res = await dispatchHostCall(PKG, name, { draftId: DRAFT, patch: {} });
			expect(res.isError).toBe(true);
			expect(res.structuredContent).toEqual({ ok: false, reason: 'scope-denied' });
		}

		expect(commit).not.toHaveBeenCalled();
		expect(reject).not.toHaveBeenCalled();
		expect(retry).not.toHaveBeenCalled();
		expect(update).not.toHaveBeenCalled();
	});

	it('rejects a missing draftId before checking scope', async () => {
		withScope(true);

		const res = await dispatchHostCall(PKG, 'host.paActions.commit', {});

		expect(res.isError).toBe(true);
		expect(commit).not.toHaveBeenCalled();
	});

	it('surfaces a Rust error as a non-ok envelope (e.g. retry on a non-failed row)', async () => {
		withScope(true);
		retry.mockRejectedValue(new Error('draft is not in the failed state') as never);

		const res = await dispatchHostCall(PKG, 'host.paActions.retry', { draftId: DRAFT });

		expect(res.isError).toBe(true);
		expect(res.structuredContent).toEqual({
			ok: false,
			error: 'draft is not in the failed state',
		});
	});
});

describe('host.sendToActiveSession (Issue #127)', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		useTerminalStore.setState({ tabs: [], activeId: null, rehydrated: true });
	});

	it('rejects a missing prompt/text argument', async () => {
		withScope(true);
		const res = await dispatchHostCall(PKG, 'host.sendToActiveSession', {});
		expect(res.isError).toBe(true);
		expect(res.content[0].text).toContain('missing required `prompt` or `text` argument');
	});

	it('denies execution when engine:invoke scope is missing', async () => {
		withScope(false);
		const res = await dispatchHostCall(PKG, 'host.sendToActiveSession', { prompt: 'hello Chi' });
		expect(res.isError).toBe(true);
		expect(res.structuredContent).toEqual({ ok: false, reason: 'scope-denied' });
		expect(mockPtyWrite).not.toHaveBeenCalled();
	});

	it('returns no-active-session when no terminal tabs exist', async () => {
		withScope(true);
		const res = await dispatchHostCall(PKG, 'host.sendToActiveSession', { prompt: 'hello Chi' });
		expect(res.structuredContent).toEqual({ ok: false, reason: 'no-active-session' });
		expect(mockPtyWrite).not.toHaveBeenCalled();
	});

	it('writes bracketed prompt and returns threadId when active session exists', async () => {
		withScope(true);
		useTerminalStore.setState({
			tabs: [
				{
					id: 'tab-1',
					ptyId: 'pty-123',
					claudeSessionId: 'sess-abc',
					status: 'running',
					title: 'Terminal 1',
					spec: { cwd: '/tmp', cmd: ['bash'] },
					exitCode: null,
					createdAt: 0,
					owner: { kind: 'sidepane' },
				},
			],
			activeId: 'tab-1',
			rehydrated: true,
		});

		const res = await dispatchHostCall(PKG, 'host.sendToActiveSession', {
			prompt: 'generate visual',
		});
		expect(res.isError).toBeFalsy();
		expect(res.structuredContent).toEqual({ ok: true, threadId: 'sess-abc' });
		expect(mockPtyWrite).toHaveBeenCalledWith('pty-123', '\x1b[200~generate visual\x1b[201~');
		expect(mockPtyWrite).toHaveBeenCalledWith('pty-123', '\r');
	});
});
