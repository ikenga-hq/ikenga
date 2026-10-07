import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const t = vi.hoisted(() => ({
	browser: true,
	perm: 'default' as string,
	request: vi.fn(),
	granted: vi.fn(async () => false),
}));

vi.mock('@/lib/transport', () => ({
	isBrowserHost: () => t.browser,
	browserNotificationPermission: () => t.perm,
	isNotificationPermissionGranted: t.granted,
	requestNotificationPermission: t.request,
	sendNotification: vi.fn(),
	listen: vi.fn(async () => () => {}),
}));
vi.mock('@/lib/tauri-cmd', () => ({
	settingsGet: vi.fn(async () => null),
	settingsSet: vi.fn(async () => {}),
}));
vi.mock('@/shell/notifications/actions', () => ({
	decideHookRequest: vi.fn(),
	hostDecideBlock: () => null,
	refreshHostDecideBlock: async () => null,
}));

import { PermissionInbox } from './permission-inbox';

beforeEach(() => {
	t.browser = true;
	t.perm = 'default';
	t.request.mockReset();
	t.request.mockImplementation(async () => {
		t.perm = 'granted';
		return 'granted';
	});
	t.granted.mockClear();
});
afterEach(cleanup);

describe('PermissionInbox notification permission', () => {
	it('browser: no prompt on mount, an explicit enable button instead', async () => {
		await act(async () => {
			render(<PermissionInbox sessionId="s1" />);
		});
		expect(t.request).not.toHaveBeenCalled();
		const button = screen.getByRole('button', { name: 'Enable notifications' });
		await act(async () => {
			fireEvent.click(button);
		});
		expect(t.request).toHaveBeenCalledOnce();
		expect(screen.queryByRole('button', { name: 'Enable notifications' })).toBeNull();
	});

	it('browser: no button once permission is decided or unsupported', async () => {
		t.perm = 'unsupported';
		await act(async () => {
			render(<PermissionInbox sessionId="s1" />);
		});
		expect(screen.queryByRole('button', { name: 'Enable notifications' })).toBeNull();
	});

	it('desktop: still requests permission on mount, no button', async () => {
		t.browser = false;
		await act(async () => {
			render(<PermissionInbox sessionId="s1" />);
		});
		expect(t.request).toHaveBeenCalledOnce();
		expect(screen.queryByRole('button', { name: 'Enable notifications' })).toBeNull();
	});
});
