// Verb-level tests for the approve-gate verbs (host.paActions.*, WP-18a + the
// outbox redesign's frozen contract G-CONTRACT, plans/outbox-redesign/01-plan.md).
//
// Covers the thin wrappers over the tested `pa_actions_*` Rust commands: the
// `engine:invoke` scope gate (shared with host.sendToActiveSession), the draftId
// requirement, that an authorized call threads through to the right Rust command
// with exactly the contract's arguments, and that a thrown Rust error surfaces as
// a non-ok envelope. The share-mode cases (the commit binds to the row shown, a
// broker refusal reaches the pkg in plain words, a Reviewer's refused list) moved
// here from the retired /outbox/approvals panel tests. The Rust `pa_actions_*` are
// mocked at the `@/lib/tauri-cmd` boundary — this layer only proves the
// dispatcher's guards fire and dispatch is wired.

import { beforeEach, describe, expect, it, vi } from 'vitest';

const share = vi.hoisted(() => ({ current: null as null | { role: string } }));

vi.mock('@/lib/transport', async (importOriginal) => ({
	...(await importOriginal<typeof import('@/lib/transport')>()),
	currentShare: () => share.current,
}));

vi.mock('@/lib/tauri-cmd', () => ({
	dbQuery: vi.fn(),
	dbExec: vi.fn(),
	paActionsCommit: vi.fn(),
	paActionsList: vi.fn(),
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
	type PaActionDraftRow,
	paActionsCommit,
	paActionsList,
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
const listRows = vi.mocked(paActionsList);
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
	share.current = null;
});

const ROW: PaActionDraftRow = {
	id: 'ruby-reply:em-1001',
	batchId: 'b',
	actionId: 'ruby/email-reply',
	status: 'awaiting',
	channel: 'smtp',
	payloadJson: JSON.stringify({ item: { id: 'ruby-reply:em-1001', subject: 'Re: Invoice 42' } }),
	editedJson: null,
	scheduledAt: null,
	createdAt: '2026-10-20 09:00:00',
	committedAt: null,
	sentAt: null,
	claimedAt: null,
	attempts: 0,
	lastAttemptAt: null,
	errorText: null,
	externalId: null,
	deliveryStatus: null,
	deliveryCheckedAt: null,
	rejectReason: null,
};
const SEEN = {
	channel: ROW.channel,
	payloadJson: ROW.payloadJson,
	editedJson: ROW.editedJson,
	scheduledAt: ROW.scheduledAt,
};

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

	it('update threads the contract patch keys through to paActionsUpdate and drops the rest', async () => {
		withScope(true);
		update.mockResolvedValue(undefined as never);

		const patch = {
			subject: 'New subject',
			body: 'New body',
			subjectB: 'B',
			preheader: 'pre',
			firstComment: 'first',
			hashtags: ['#a'],
			media: { kind: 'single', imageUrls: ['https://x/1.png'] },
			scheduledIso: null,
		};
		const res = await dispatchHostCall(PKG, 'host.paActions.update', {
			draftId: DRAFT,
			patch: { ...patch, junk: 1, status: 'committed' },
		});

		expect(update).toHaveBeenCalledWith(DRAFT, patch);
		expect(res.structuredContent).toEqual({ ok: true });
	});

	it('update refuses a wrongly-typed contract key without dispatching', async () => {
		withScope(true);

		const res = await dispatchHostCall(PKG, 'host.paActions.update', {
			draftId: DRAFT,
			patch: { hashtags: '#a' },
		});

		expect(res.isError).toBe(true);
		expect(res.content[0].text).toContain('patch.hashtags');
		expect(update).not.toHaveBeenCalled();
	});

	it('reject passes a reason through when given', async () => {
		withScope(true);
		reject.mockResolvedValue(undefined as never);

		await dispatchHostCall(PKG, 'host.paActions.reject', {
			draftId: DRAFT,
			reason: ' Wrong tone ',
		});
		expect(reject).toHaveBeenCalledWith(DRAFT, 'Wrong tone');

		const bad = await dispatchHostCall(PKG, 'host.paActions.reject', { draftId: DRAFT, reason: 7 });
		expect(bad.isError).toBe(true);
		expect(reject).toHaveBeenCalledTimes(1);
	});

	it('list returns the rows, the status filter and whether this is a share', async () => {
		withScope(true);
		listRows.mockResolvedValue([ROW]);

		const res = await dispatchHostCall(PKG, 'host.paActions.list', {});
		expect(listRows).toHaveBeenCalledWith(undefined);
		expect(res.structuredContent).toEqual({ ok: true, rows: [ROW], inShare: false });

		share.current = { role: 'operator' };
		const shared = await dispatchHostCall(PKG, 'host.paActions.list', { status: 'sent' });
		expect(listRows).toHaveBeenLastCalledWith('sent');
		expect(shared.structuredContent).toEqual({ ok: true, rows: [ROW], inShare: true });

		const bad = await dispatchHostCall(PKG, 'host.paActions.list', { status: 3 });
		expect(bad.isError).toBe(true);
		expect(listRows).toHaveBeenCalledTimes(2);
	});

	it('denies every verb without the engine:invoke scope (scope-denied, no dispatch)', async () => {
		withScope(false);

		for (const name of [
			'host.paActions.list',
			'host.paActions.commit',
			'host.paActions.reject',
			'host.paActions.retry',
			'host.paActions.update',
		]) {
			const res = await dispatchHostCall(PKG, name, { draftId: DRAFT, patch: { body: 'x' } });
			expect(res.isError).toBe(true);
			expect(res.structuredContent).toEqual({ ok: false, reason: 'scope-denied' });
		}

		expect(listRows).not.toHaveBeenCalled();
		expect(commit).not.toHaveBeenCalled();
		expect(reject).not.toHaveBeenCalled();
		expect(retry).not.toHaveBeenCalled();
		expect(update).not.toHaveBeenCalled();
	});

	it('an unknown paActions verb is an error, not a fall-through', async () => {
		withScope(true);
		const res = await dispatchHostCall(PKG, 'host.paActions.pause', { draftId: DRAFT });
		expect(res.isError).toBe(true);
		expect(res.content[0].text).toContain('unknown host tool');
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
			message: 'draft is not in the failed state',
		});
	});
});

describe('host.paActions.* inside a share (moved from the /outbox/approvals panel tests)', () => {
	beforeEach(() => {
		share.current = { role: 'operator' };
	});

	it('commit carries the row that was shown, verbatim', async () => {
		withScope(true);
		commit.mockResolvedValue(undefined as never);

		const res = await dispatchHostCall(PKG, 'host.paActions.commit', {
			draftId: ROW.id,
			seen: { ...SEEN, status: 'awaiting' },
		});

		expect(commit).toHaveBeenCalledWith(ROW.id, SEEN);
		expect(res.structuredContent).toEqual({ ok: true });
	});

	it('seen may be the whole row from list (with broker decorations): only the hashed columns go on', async () => {
		withScope(true);
		commit.mockResolvedValue(undefined as never);
		const listed = {
			...ROW,
			editedJson: '{"body":"x"}',
			ledgerApproval: null,
			outreachStatus: { state: 'queued' },
		};

		await dispatchHostCall(PKG, 'host.paActions.commit', { draftId: ROW.id, seen: listed });

		expect(commit).toHaveBeenCalledWith(ROW.id, { ...SEEN, editedJson: '{"body":"x"}' });
	});

	it('outside a share a commit without seen carries only the draft id', async () => {
		share.current = null;
		withScope(true);
		commit.mockResolvedValue(undefined as never);

		await dispatchHostCall(PKG, 'host.paActions.commit', { draftId: ROW.id });
		expect(commit.mock.calls[0]).toEqual([ROW.id]);
	});

	it('a malformed seen is refused before anything is sent', async () => {
		withScope(true);
		const res = await dispatchHostCall(PKG, 'host.paActions.commit', {
			draftId: ROW.id,
			seen: { channel: 'smtp' },
		});
		expect(res.isError).toBe(true);
		expect(commit).not.toHaveBeenCalled();
	});

	it('a broker refusal reaches the pkg raw (with its code) and as a plain sentence', async () => {
		withScope(true);
		const raw =
			"approval_refused: Nothing was approved: you are not on the box's list of people who may approve sends.";
		commit.mockRejectedValue(new Error(raw) as never);

		const res = await dispatchHostCall(PKG, 'host.paActions.commit', {
			draftId: ROW.id,
			seen: SEEN,
		});

		expect(res.isError).toBe(true);
		expect(res.structuredContent).toEqual({
			ok: false,
			error: raw,
			message:
				"Nothing was approved: you are not on the box's list of people who may approve sends.",
		});
	});

	it('"approval service unreachable" is a failed commit, not a silent success', async () => {
		withScope(true);
		commit.mockRejectedValue(
			new Error(
				'approval_unavailable: Nothing was approved: the box’s approval service is not reachable.'
			) as never
		);
		const res = await dispatchHostCall(PKG, 'host.paActions.commit', {
			draftId: ROW.id,
			seen: SEEN,
		});
		expect(res.structuredContent?.ok).toBe(false);
		expect(res.structuredContent?.message).toMatch(/approval service is not reachable/);
	});

	it("a Reviewer's refused list is an error with the broker's reason, not an empty queue", async () => {
		share.current = { role: 'reviewer' };
		withScope(true);
		listRows.mockRejectedValue(
			new Error(
				'forbidden: reviewing and approving drafts in a shared project needs the Operator role'
			) as never
		);

		const res = await dispatchHostCall(PKG, 'host.paActions.list', {});

		expect(res.isError).toBe(true);
		expect(res.structuredContent).toMatchObject({ ok: false });
		expect(res.structuredContent?.message).toMatch(/needs the Operator role/);
		expect(res.structuredContent).not.toHaveProperty('rows');
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
