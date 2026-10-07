// WP-40b — `notificationActionButtons` is the single place popover rows and
// toast pills decide what "Allow once", "Review", "Open log" etc. actually
// do. Covers every `NotificationAction.kind` the producers emit
// (`src-tauri/src/notifications/producers.rs`) plus the no-action /
// unknown-kind fallbacks.

import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { NotificationRow } from '@/lib/tauri-cmd';

const mocks = vi.hoisted(() => ({
	navigateFocused: vi.fn(),
	addTab: vi.fn(),
	iykeFetch: vi.fn(() => Promise.resolve(new Response(null, { status: 204 }))),
	permissionDecide: vi.fn((_id: number, _d: string) => Promise.resolve({ resolved: true })),
	accessStatus: vi.fn(),
	accessRoutingGet: vi.fn(),
	remote: false,
	decideRemote: vi.fn(),
}));

vi.mock('@/lib/transport', () => ({ isRemoteWebSession: () => mocks.remote }));
vi.mock('@/lib/iyke/terminal-hooks', () => ({ decideHookGateRemote: mocks.decideRemote }));

vi.mock('@/lib/access/client', async (orig) => ({
	...(await orig<typeof import('@/lib/access/client')>()),
	permissionDecide: mocks.permissionDecide,
	accessStatus: mocks.accessStatus,
	accessRoutingGet: mocks.accessRoutingGet,
}));

vi.mock('@/lib/panes/pane-store', () => ({
	usePaneStore: {
		getState: () => ({
			focusedId: 'pane-1',
			navigateFocused: mocks.navigateFocused,
			addTab: mocks.addTab,
		}),
	},
}));

vi.mock('@/lib/iyke/client', () => ({
	iykeFetch: mocks.iykeFetch,
}));

import {
	ACP_ASK_ANSWERABLE_MS,
	ASK_ALREADY_OVER,
	decidePermissionRow,
	HOOK_GATE_ANSWERABLE_MS,
	hostDecideBlock,
	isPermissionAskLive,
	notificationActionButtons,
	postHookDecision,
	refreshHostDecideBlock,
	setHostDecideBlock,
} from './actions';

/** Rows below are created at t=0; "now" inside the hold window. */
const NOW = 1_000;

function row(overrides: Partial<NotificationRow>): NotificationRow {
	return {
		id: 1,
		kind: 'permission',
		title: 't',
		body: null,
		action: null,
		source: 'test',
		dedupeKey: null,
		count: 1,
		createdAt: 0,
		updatedAt: 0,
		readAt: null,
		resolvedAt: null,
		...overrides,
	};
}

beforeEach(() => {
	mocks.navigateFocused.mockClear();
	mocks.addTab.mockClear();
	mocks.iykeFetch.mockClear();
	mocks.permissionDecide.mockClear();
	mocks.accessStatus.mockReset();
	mocks.accessRoutingGet.mockReset();
});

describe('notificationActionButtons', () => {
	it('permission.decide: Allow once / Deny go through permission_decide (G-ACCESS §5.5)', async () => {
		const buttons = notificationActionButtons(
			row({
				kind: 'permission',
				action: { kind: 'permission.decide', via: 'hooks', requestId: 'req-1', terminalId: 't-1' },
			}),
			NOW
		);
		expect(buttons.map((b) => b.label)).toEqual(['Allow once', 'Deny']);
		expect(buttons[0]?.variant).toBe('primary');
		expect(buttons[1]?.variant).toBe('ghost');

		buttons[0]?.run();
		expect(mocks.permissionDecide).toHaveBeenCalledWith(1, 'allow_once');
		buttons[1]?.run();
		expect(mocks.permissionDecide).toHaveBeenLastCalledWith(1, 'deny');
		await Promise.resolve();
		expect(mocks.iykeFetch).not.toHaveBeenCalled();
	});

	it('decidePermissionRow falls back to the hooks route only for a non-final failure', async () => {
		mocks.permissionDecide.mockRejectedValueOnce(new Error('internal: not running'));
		expect(await decidePermissionRow(1, 'allow_once', 'req-1')).toBeNull();
		expect(mocks.iykeFetch).toHaveBeenCalledWith(
			'/iyke/hooks/decision',
			expect.objectContaining({
				method: 'POST',
				body: JSON.stringify({ requestId: 'req-1', decision: 'approved' }),
			})
		);
		mocks.iykeFetch.mockClear();
		for (const err of [
			'routing_refused: answered on another device',
			'conflict: already answered',
			'owner_approval_required: owner',
		]) {
			mocks.permissionDecide.mockRejectedValueOnce(new Error(err));
			expect(await decidePermissionRow(1, 'deny', 'req-1')).toBe(
				err.slice(err.indexOf(':') + 1).trim()
			);
		}
		expect(mocks.iykeFetch).not.toHaveBeenCalled();
	});

	// Review WP78a-R5: a 2xx that reached no held gate is not an answer.
	it('postHookDecision answers a daemon terminal through the daemon arm in a browser', async () => {
		mocks.remote = true;
		try {
			mocks.decideRemote.mockResolvedValueOnce(true);
			expect(await postHookDecision('perm-1', 'approved')).toBeNull();
			expect(mocks.decideRemote).toHaveBeenCalledWith('perm-1', 'approved');
			// Already answered / timed out: never presented as decided.
			mocks.decideRemote.mockResolvedValueOnce(false);
			expect(await postHookDecision('perm-1', 'denied')).toBe(ASK_ALREADY_OVER);
			// A refusal is shown, not swallowed.
			mocks.decideRemote.mockRejectedValueOnce(new Error('forbidden: missing approve'));
			expect(await postHookDecision('perm-1', 'approved')).toContain('missing approve');
			expect(mocks.iykeFetch).not.toHaveBeenCalled();
		} finally {
			mocks.remote = false;
		}
	});

	it('postHookDecision reads `gated: false` as "already over", not answered', async () => {
		const json = (body: unknown) =>
			new Response(JSON.stringify(body), {
				status: 200,
				headers: { 'Content-Type': 'application/json' },
			});
		mocks.iykeFetch.mockResolvedValueOnce(json({ recorded: true, gated: false }));
		expect(await postHookDecision('req-1', 'approved')).toBe(ASK_ALREADY_OVER);
		mocks.iykeFetch.mockResolvedValueOnce(json({ recorded: true, gated: true }));
		expect(await postHookDecision('req-1', 'approved')).toBeNull();
		// An older backend (no body / no flag) still reads as answered.
		expect(await postHookDecision('req-1', 'denied')).toBeNull();
	});

	it('a desktop routed to another device offers no Allow / Deny (§5.1)', async () => {
		mocks.accessStatus.mockResolvedValueOnce({ store: 'ok', caps: ['files', 'sessions'] });
		mocks.accessRoutingGet.mockResolvedValueOnce({
			mode: 'this_device',
			deviceId: 'p',
			deviceName: 'Pixel 9',
		});
		expect(await refreshHostDecideBlock()).toBe('Answer on Pixel 9 (this device only)');
		expect(hostDecideBlock()).toMatch(/Pixel 9/);
		const decide = {
			kind: 'permission.decide',
			via: 'hooks',
			requestId: 'r',
			terminalId: 't-1',
		} as const;
		expect(notificationActionButtons(row({ action: decide }), NOW).map((b) => b.label)).toEqual([
			'Open terminal',
		]);
		mocks.accessStatus.mockResolvedValueOnce({ store: 'ok', caps: ['approve'] });
		expect(await refreshHostDecideBlock()).toBeNull();
		setHostDecideBlock(null);
	});

	it('permission.decide: a resolved row offers no live decision, only Open terminal', () => {
		const buttons = notificationActionButtons(
			row({
				kind: 'permission',
				resolvedAt: 500,
				readAt: 500,
				action: { kind: 'permission.decide', via: 'hooks', requestId: 'req-1', terminalId: 't-1' },
			}),
			NOW
		);
		expect(buttons.map((b) => b.label)).toEqual(['Open terminal']);
		buttons[0]?.run();
		expect(mocks.iykeFetch).not.toHaveBeenCalled();
		expect(mocks.addTab).toHaveBeenCalledWith('pane-1', { kind: 'terminal', sessionId: 't-1' });
	});

	it('permission.decide: resolved without a terminal renders no buttons', () => {
		const buttons = notificationActionButtons(
			row({
				kind: 'permission',
				resolvedAt: 500,
				action: { kind: 'permission.decide', via: 'hooks', requestId: 'req-1', terminalId: null },
			}),
			NOW
		);
		expect(buttons).toEqual([]);
	});

	it('permission.decide: an expired hold offers no live decision even without resolvedAt', () => {
		const decide = {
			kind: 'permission.decide',
			via: 'hooks',
			requestId: 'req-1',
			terminalId: null,
		} as const;
		expect(notificationActionButtons(row({ action: decide }), HOOK_GATE_ANSWERABLE_MS + 1)).toEqual(
			[]
		);
	});

	it('isPermissionAskLive: read-but-pending stays live; an older row without resolvedAt uses read state', () => {
		expect(isPermissionAskLive(row({ readAt: 500, resolvedAt: null }), NOW)).toBe(true);
		const legacy = row({ readAt: 500 });
		delete (legacy as Partial<NotificationRow>).resolvedAt;
		expect(isPermissionAskLive(legacy, NOW)).toBe(false);
		const legacyUnread = row({});
		delete (legacyUnread as Partial<NotificationRow>).resolvedAt;
		expect(isPermissionAskLive(legacyUnread, NOW)).toBe(true);
	});

	it('an ACP ask is answerable inline while the round-trip waits (WP-75), then open-only', () => {
		const acp = {
			kind: 'permission.decide',
			via: 'acp',
			threadId: 'th-9',
			requestId: 'r-9',
		} as const;
		const live = notificationActionButtons(row({ kind: 'permission', action: acp }), NOW);
		expect(live.map((b) => b.label)).toEqual([
			'Allow once',
			'Always for this project',
			'Deny',
			'Open thread',
		]);
		live[1]?.run();
		expect(mocks.permissionDecide).toHaveBeenCalledWith(1, 'allow_always_project');

		const buttons = notificationActionButtons(
			row({ kind: 'permission', action: acp, resolvedAt: 500 }),
			NOW
		);
		expect(buttons.map((b) => b.label)).toEqual(['Open thread']);
		expect(
			notificationActionButtons(
				row({ kind: 'permission', action: acp }),
				ACP_ASK_ANSWERABLE_MS + 1
			).map((b) => b.label)
		).toEqual(['Open thread']);
		buttons[0]?.run();
		expect(mocks.iykeFetch).not.toHaveBeenCalled();
		expect(mocks.addTab).toHaveBeenCalledWith('pane-1', { kind: 'terminal', sessionId: 'th-9' });
	});

	it('open.terminal opens a terminal pane keyed by terminalId, falling back to sessionId', () => {
		const withTerminal = notificationActionButtons(
			row({ action: { kind: 'open.terminal', terminalId: 'term-1', sessionId: null } })
		);
		withTerminal[0]?.run();
		expect(mocks.addTab).toHaveBeenCalledWith('pane-1', { kind: 'terminal', sessionId: 'term-1' });

		mocks.addTab.mockClear();
		const sessionOnly = notificationActionButtons(
			row({ action: { kind: 'open.terminal', terminalId: null, sessionId: 'sess-1' } })
		);
		sessionOnly[0]?.run();
		expect(mocks.addTab).toHaveBeenCalledWith('pane-1', { kind: 'terminal', sessionId: 'sess-1' });
	});

	it('open.terminal with neither id renders no button', () => {
		const buttons = notificationActionButtons(
			row({ action: { kind: 'open.terminal', terminalId: null, sessionId: null } })
		);
		expect(buttons).toEqual([]);
	});

	it('open.thread opens a terminal pane keyed by threadId', () => {
		const buttons = notificationActionButtons(
			row({ action: { kind: 'open.thread', threadId: 'thread-1', requestId: 'req-2' } })
		);
		expect(buttons.map((b) => b.label)).toEqual(['Open thread']);
		buttons[0]?.run();
		expect(mocks.addTab).toHaveBeenCalledWith('pane-1', {
			kind: 'terminal',
			sessionId: 'thread-1',
		});
	});

	it('open.chi_run labels "Open log" on failure, "Open artifact" on success, both route to /automations', () => {
		const failed = notificationActionButtons(
			row({ kind: 'run_failed', action: { kind: 'open.chi_run', runId: 'r1', status: 'failed' } })
		);
		expect(failed.map((b) => b.label)).toEqual(['Open log']);
		failed[0]?.run();
		expect(mocks.navigateFocused).toHaveBeenLastCalledWith('/automations?view=runs');

		const done = notificationActionButtons(
			row({ kind: 'run_finished', action: { kind: 'open.chi_run', runId: 'r2', status: 'done' } })
		);
		expect(done.map((b) => b.label)).toEqual(['Open artifact']);
	});

	it('open.release_notes routes to /settings/about', () => {
		const buttons = notificationActionButtons(
			row({
				kind: 'update',
				action: { kind: 'open.release_notes', source: 'shell', version: '0.9.1' },
			})
		);
		buttons[0]?.run();
		expect(mocks.navigateFocused).toHaveBeenCalledWith('/settings/about');
	});

	it('open.pkg_updates routes to the Ngwa updates filter', () => {
		const buttons = notificationActionButtons(
			row({
				kind: 'update',
				action: { kind: 'open.pkg_updates', pkgId: 'com.ikenga.iyke', version: '1.0.0' },
			})
		);
		buttons[0]?.run();
		expect(mocks.navigateFocused).toHaveBeenCalledWith('/packages?filter=updates');
	});

	it('open.violations routes to the Ngwa review filter as the primary action', () => {
		const buttons = notificationActionButtons(
			row({
				kind: 'violation',
				action: { kind: 'open.violations', pkgId: 'com.ikenga.pkg-browser' },
			})
		);
		expect(buttons[0]?.variant).toBe('primary');
		buttons[0]?.run();
		expect(mocks.navigateFocused).toHaveBeenCalledWith('/packages?filter=review');
	});

	it('invite with no action yet (no D-05 producer) still offers Open People', () => {
		const buttons = notificationActionButtons(row({ kind: 'invite', action: null }));
		expect(buttons.map((b) => b.label)).toEqual(['Open People']);
		buttons[0]?.run();
		expect(mocks.navigateFocused).toHaveBeenCalledWith('/settings/people');
	});

	it('no action and a non-invite kind renders no buttons', () => {
		expect(notificationActionButtons(row({ kind: 'run_finished', action: null }))).toEqual([]);
	});

	it('an unrecognized action kind renders no buttons for a non-invite row', () => {
		const buttons = notificationActionButtons(
			row({ kind: 'update', action: { kind: 'some.future.action', foo: 'bar' } })
		);
		expect(buttons).toEqual([]);
	});
});
