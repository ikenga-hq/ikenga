// Daemon asks (remote-access): a browser session answers a held hook gate from
// the bell, a Companion card or the home tile through the SAME `permission`
// row the daemon records for it (`server/hook_asks.rs`), exactly as the
// desktop does — `permission_decide` on the row, never the legacy
// `term_hooks_decide` arm while a row is open, and never a second try after the
// daemon refused on purpose.

import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { NotificationRow } from '@/lib/tauri-cmd';

const mocks = vi.hoisted(() => ({
	permissionDecide: vi.fn((_id: number, _d: string) => Promise.resolve({ resolved: true })),
	decideRemote: vi.fn(),
	notificationsList: vi.fn(),
	iykeFetch: vi.fn(),
}));

vi.mock('@/lib/transport', () => ({ isRemoteWebSession: () => true }));
vi.mock('@/lib/iyke/terminal-hooks', () => ({ decideHookGateRemote: mocks.decideRemote }));
vi.mock('@/lib/iyke/client', () => ({ iykeFetch: mocks.iykeFetch }));
vi.mock('@/lib/access/client', async (orig) => ({
	...(await orig<typeof import('@/lib/access/client')>()),
	permissionDecide: mocks.permissionDecide,
	accessStatus: vi.fn(),
	accessRoutingGet: vi.fn(),
}));
vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	notificationsList: mocks.notificationsList,
}));
vi.mock('@/lib/panes/pane-store', () => ({
	usePaneStore: { getState: () => ({ focusedId: 'p', navigateFocused: vi.fn(), addTab: vi.fn() }) },
}));

import { ASK_ALREADY_OVER, decideHookRequest, notificationActionButtons } from './actions';

/** The row the daemon records for a held gate (`hook_ask::permission_from_hook_gate`
 *  plus the hold's `expiresAtMs`). */
function daemonRow(overrides: Partial<NotificationRow> = {}): NotificationRow {
	return {
		id: 7,
		kind: 'permission',
		title: 'Claude wants to use Bash',
		body: 'rm -rf /tmp/x · terminal t-1',
		action: {
			kind: 'permission.decide',
			via: 'hooks',
			requestId: 'perm-abc',
			terminalId: 't-1',
			expiresAtMs: 30_000,
		},
		source: 'iyke.hooks',
		dedupeKey: 'permission:hook:perm-abc',
		count: 1,
		createdAt: 0,
		updatedAt: 0,
		readAt: null,
		resolvedAt: null,
		...overrides,
	};
}

beforeEach(() => {
	for (const m of Object.values(mocks)) m.mockReset();
	mocks.permissionDecide.mockResolvedValue({ resolved: true });
});

describe('answering a daemon terminal ask from a browser session', () => {
	it('a card / tile answer goes through the ask’s row, not the legacy arm', async () => {
		mocks.notificationsList.mockResolvedValue([daemonRow()]);
		expect(await decideHookRequest('perm-abc', 'approved')).toBeNull();
		expect(mocks.permissionDecide).toHaveBeenCalledWith(7, 'allow_once');
		expect(mocks.decideRemote).not.toHaveBeenCalled();

		expect(await decideHookRequest('perm-abc', 'denied')).toBeNull();
		expect(mocks.permissionDecide).toHaveBeenLastCalledWith(7, 'deny');
	});

	it('the bell’s Allow / Deny on a live daemon row call permission_decide on that row', () => {
		const buttons = notificationActionButtons(daemonRow(), 1_000, null);
		expect(buttons.map((b) => b.label)).toEqual(['Allow once', 'Deny']);
		buttons[0]?.run();
		expect(mocks.permissionDecide).toHaveBeenCalledWith(7, 'allow_once');
	});

	it('an ask the daemon already resolved (answered, timed out, terminal gone) offers no Allow / Deny', () => {
		const resolved = daemonRow({ resolvedAt: 5_000 });
		expect(notificationActionButtons(resolved, 6_000, null).map((b) => b.label)).toEqual([
			'Open terminal',
		]);
	});

	it('with no row recorded yet the legacy arm answers, and reports an over ask as over', async () => {
		mocks.notificationsList.mockResolvedValue([]);
		mocks.decideRemote.mockResolvedValueOnce(true);
		expect(await decideHookRequest('perm-abc', 'approved')).toBeNull();
		expect(mocks.permissionDecide).not.toHaveBeenCalled();
		expect(mocks.decideRemote).toHaveBeenCalledWith('perm-abc', 'approved');

		mocks.decideRemote.mockResolvedValueOnce(false);
		expect(await decideHookRequest('perm-abc', 'approved')).toBe(ASK_ALREADY_OVER);
	});

	it('a decision the daemon refused on purpose is shown, never retried on the legacy arm', async () => {
		mocks.notificationsList.mockResolvedValue([daemonRow()]);
		for (const refusal of [
			'conflict: this ask was already answered or timed out',
			'owner_approval_required: asks that touch secrets go to the Owner',
			'forbidden: missing approve',
		]) {
			mocks.permissionDecide.mockRejectedValueOnce(new Error(refusal));
			const shown = await decideHookRequest('perm-abc', 'approved');
			expect(shown).toBe(refusal.slice(refusal.indexOf(':') + 1).trim());
		}
		expect(mocks.decideRemote).not.toHaveBeenCalled();
	});
});
