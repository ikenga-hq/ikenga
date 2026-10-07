// plans/pwa S4 §3: the per-event toggles, and the desktop app's one line.

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const transport = vi.hoisted(() => ({ tauri: true }));
vi.mock('@/lib/transport', () => ({ isTauri: () => transport.tauri }));
const api = vi.hoisted(() => ({ accessPushConfig: vi.fn(), accessPushList: vi.fn() }));
vi.mock('@/lib/access/client', async (orig) => ({
	...(await orig<typeof import('@/lib/access/client')>()),
	...api,
}));

import {
	NotificationsSettings,
	PUSH_GROUPS,
	toggleGroup,
	visibleGroups,
} from './notifications-settings';

afterEach(cleanup);

describe('Settings › Notifications', () => {
	it('shows only the groups this credential may receive (updates: admins)', () => {
		expect(visibleGroups(['run_finished', 'invite']).map((g) => g.id)).toEqual(['runs', 'people']);
		expect(visibleGroups(['permission', 'update']).map((g) => g.id)).toEqual([
			'approvals',
			'updates',
		]);
	});

	it('toggles a whole group, never adding a kind the credential lacks', () => {
		const runs = PUSH_GROUPS.find((g) => g.id === 'runs')!;
		const people = PUSH_GROUPS.find((g) => g.id === 'people')!;
		expect(toggleGroup([], runs, true, ['run_finished', 'run_failed'])).toEqual([
			'run_finished',
			'run_failed',
		]);
		expect(toggleGroup(['run_finished', 'invite'], runs, false, ['run_finished'])).toEqual([
			'invite',
		]);
		expect(toggleGroup([], people, true, ['invite'])).toEqual(['invite']);
	});

	it('on the desktop app says where notifications go and calls no push arm', () => {
		transport.tauri = true;
		render(<NotificationsSettings />);
		expect(screen.getByText(/turn them on from Ikenga opened in that browser/)).toBeTruthy();
		expect(api.accessPushConfig).not.toHaveBeenCalled();
		expect(api.accessPushList).not.toHaveBeenCalled();
	});
});
