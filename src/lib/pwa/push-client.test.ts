// plans/pwa S4 §4: turning push on asks for permission first (inside the
// click), subscribes with the server's key, and registers the subscription;
// a key change re-subscribes; turning off removes both halves. And the deep
// link's live lookups.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({
	accessPushConfig: vi.fn(),
	accessPushSubscribe: vi.fn(),
	accessPushUnsubscribe: vi.fn(),
	accessPushList: vi.fn(),
	accessPairPending: vi.fn(),
}));
vi.mock('@/lib/access/client', () => api);
vi.mock('@/lib/tauri-cmd', () => ({ notificationsList: vi.fn() }));

import { handlePushOpen, resolvePushOpen, usePushOpenStore } from './deeplink';
import {
	b64urlToBytes,
	bytesToB64url,
	disablePush,
	enablePush,
	labelFromUserAgent,
	readStoredSub,
	reconcilePush,
	SUB_STORAGE_KEY,
} from './push-client';

const KEY_A = bytesToB64url(new Uint8Array(65).fill(4));
const KEY_B = bytesToB64url(new Uint8Array(65).fill(9));

class FakeSub {
	unsubscribe = vi.fn(async () => true);
	constructor(
		public endpoint: string,
		public key: string
	) {}
	get options() {
		return { applicationServerKey: b64urlToBytes(this.key).buffer, userVisibleOnly: true };
	}
	toJSON() {
		return { endpoint: this.endpoint, keys: { p256dh: 'P256', auth: 'AUTH' } };
	}
}

let current: FakeSub | null;
const pushManager = {
	getSubscription: vi.fn(async () => current),
	subscribe: vi.fn(async (opts: { applicationServerKey: Uint8Array }) => {
		current = new FakeSub(
			`https://fcm.googleapis.com/fcm/send/${pushManager.subscribe.mock.calls.length}`,
			bytesToB64url(opts.applicationServerKey)
		);
		return current;
	}),
};
const reg = { pushManager, getNotifications: vi.fn(async () => []) };
const order: string[] = [];

beforeEach(() => {
	localStorage.clear();
	current = null;
	order.length = 0;
	vi.clearAllMocks();
	Object.defineProperty(navigator, 'serviceWorker', {
		configurable: true,
		value: {
			ready: Promise.resolve(reg),
			getRegistration: vi.fn(async () => reg),
			addEventListener: vi.fn(),
			removeEventListener: vi.fn(),
		},
	});
	const N = {
		permission: 'default' as NotificationPermission,
		requestPermission: vi.fn(async () => {
			order.push('permission');
			N.permission = 'granted';
			return 'granted' as NotificationPermission;
		}),
	};
	vi.stubGlobal('Notification', N);
	api.accessPushConfig.mockImplementation(async () => {
		order.push('config');
		return { enabled: true, publicKey: KEY_A, keyId: 'kid-a', kinds: ['run_finished'] };
	});
	api.accessPushSubscribe.mockResolvedValue({ subId: 'sub-1' });
	api.accessPushUnsubscribe.mockResolvedValue({ removed: 1 });
	api.accessPushList.mockResolvedValue([]);
});

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('enablePush', () => {
	it('asks for permission before anything else, then subscribes with the server key', async () => {
		const r = await enablePush();
		expect(r.ok).toBe(true);
		expect(order).toEqual(['permission', 'config']);
		const opts = pushManager.subscribe.mock.calls[0][0] as unknown as {
			userVisibleOnly: boolean;
			applicationServerKey: Uint8Array;
		};
		expect(opts.userVisibleOnly).toBe(true);
		expect(bytesToB64url(opts.applicationServerKey)).toBe(KEY_A);
		expect(api.accessPushSubscribe).toHaveBeenCalledWith(
			expect.objectContaining({
				endpoint: current?.endpoint,
				keys: { p256dh: 'P256', auth: 'AUTH' },
			})
		);
		expect(readStoredSub()).toEqual({
			subId: 'sub-1',
			endpoint: current?.endpoint,
			keyId: 'kid-a',
		});
	});

	it('stops at a refusal without touching the server', async () => {
		(Notification as unknown as { requestPermission: () => Promise<string> }).requestPermission =
			vi.fn(async () => 'denied');
		const r = await enablePush();
		expect(r).toMatchObject({ ok: false, reason: 'denied' });
		expect(api.accessPushConfig).not.toHaveBeenCalled();
		expect(pushManager.subscribe).not.toHaveBeenCalled();
	});

	it('says why when push is off on the server', async () => {
		api.accessPushConfig.mockResolvedValue({ enabled: false, reason: 'off here' });
		expect(await enablePush()).toMatchObject({ ok: false, reason: 'off', message: 'off here' });
	});

	it('replaces a subscription made with another server key', async () => {
		const old = new FakeSub('https://fcm.googleapis.com/old', KEY_B);
		current = old;
		await enablePush();
		expect(old.unsubscribe).toHaveBeenCalled();
		expect(pushManager.subscribe).toHaveBeenCalledTimes(1);
	});
});

describe('disablePush / reconcilePush', () => {
	it('turning off removes the server row and the browser subscription', async () => {
		await enablePush();
		const sub = current;
		await disablePush();
		expect(api.accessPushUnsubscribe).toHaveBeenCalledWith({ subId: 'sub-1' });
		expect(sub?.unsubscribe).toHaveBeenCalled();
		expect(localStorage.getItem(SUB_STORAGE_KEY)).toBeNull();
	});

	it('never prompts, and repairs a rotated server key', async () => {
		await enablePush();
		api.accessPushList.mockResolvedValue([{ subId: 'sub-1', kinds: ['run_failed'] }]);
		expect(await reconcilePush()).toBe('ok');
		api.accessPushConfig.mockResolvedValue({
			enabled: true,
			publicKey: KEY_B,
			keyId: 'kid-b',
			kinds: [],
		});
		api.accessPushSubscribe.mockResolvedValue({ subId: 'sub-2' });
		const requests = (Notification as unknown as { requestPermission: ReturnType<typeof vi.fn> })
			.requestPermission.mock.calls.length;
		expect(await reconcilePush()).toBe('repaired');
		expect(readStoredSub()?.keyId).toBe('kid-b');
		// The user's kinds are kept across the repair.
		expect(api.accessPushSubscribe).toHaveBeenLastCalledWith(
			expect.objectContaining({ kinds: ['run_failed'] })
		);
		expect(
			(Notification as unknown as { requestPermission: ReturnType<typeof vi.fn> }).requestPermission
				.mock.calls.length
		).toBe(requests);
	});

	it('does nothing without granted permission', async () => {
		expect(await reconcilePush()).toBe('none');
		expect(api.accessPushConfig).not.toHaveBeenCalled();
	});
});

describe('labels', () => {
	it('names browser and platform only', () => {
		expect(
			labelFromUserAgent('Mozilla/5.0 (Linux; Android 14; Pixel 9) Chrome/130 Mobile Safari/537')
		).toBe('Chrome on Android');
		expect(
			labelFromUserAgent(
				'Mozilla/5.0 (iPhone; CPU iPhone OS 17_4) Version/17 Mobile/15E Safari/604'
			)
		).toBe('Safari on iPhone');
		expect(labelFromUserAgent('Mozilla/5.0 (Windows NT 10.0) Firefox/131')).toBe(
			'Firefox on Windows'
		);
		expect(labelFromUserAgent('Mozilla/5.0 (Windows NT 10.0) Chrome/130 Edg/130')).toBe(
			'Edge on Windows'
		);
	});
});

describe('deep link lookups', () => {
	const row = (id: number, resolvedAt: number | null) => ({
		id,
		kind: 'permission',
		title: 't',
		body: null,
		action: null,
		source: 's',
		dedupeKey: null,
		count: 1,
		createdAt: 0,
		updatedAt: 0,
		readAt: null,
		resolvedAt,
	});

	it('finds an open ask, and says when it was answered elsewhere', async () => {
		const deps = {
			notificationsList: vi.fn(async () => [row(5, null), row(6, 123)]),
			accessPairPending: vi.fn(async () => []),
		};
		expect((await resolvePushOpen({ k: 'permission', r: 'n:5' }, deps as never)).kind).toBe(
			'ask-open'
		);
		expect((await resolvePushOpen({ k: 'permission', r: 'n:6' }, deps as never)).kind).toBe(
			'already-answered'
		);
		expect((await resolvePushOpen({ k: 'permission', r: 'n:9' }, deps as never)).kind).toBe(
			'already-answered'
		);
		await handlePushOpen({ k: 'permission', r: 'n:6' }, {}, deps as never);
		expect(usePushOpenStore.getState().notice).toMatch(/already answered on another device/);
	});

	it('checks a pairing request is still waiting', async () => {
		const deps = {
			notificationsList: vi.fn(),
			accessPairPending: vi.fn(async () => [{ pairingId: 'p1', state: 'awaiting_host' }]),
		};
		expect((await resolvePushOpen({ k: 'pairing', r: 'pair:p1' }, deps as never)).kind).toBe(
			'pairing-pending'
		);
		expect((await resolvePushOpen({ k: 'pairing', r: 'pair:p2' }, deps as never)).kind).toBe(
			'pairing-gone'
		);
		const navigate = vi.fn();
		await handlePushOpen({ k: 'pairing', r: 'pair:p1' }, { navigate }, deps as never);
		expect(navigate).toHaveBeenCalledWith('/settings/devices');
	});
});
