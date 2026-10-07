import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { dismissToast, useToastStore } from '@/lib/toast';

const host = vi.hoisted(() => ({ browser: false, tauri: false, invoke: vi.fn() }));

vi.mock('./index', () => ({
	isTauri: () => host.tauri,
	isBrowserHost: () => host.browser,
	getTransport: () => ({ invoke: host.invoke }),
}));

import {
	browserNotificationPermission,
	canOpenLocalPath,
	isExternalUrl,
	isNotificationPermissionGranted,
	openExternalUrl,
	openLocalPath,
	requestNotificationPermission,
	sendNotification,
} from './shims';

function toasts() {
	return useToastStore.getState().queue.map((t) => t.label);
}

beforeEach(() => {
	host.browser = false;
	host.tauri = false;
	host.invoke.mockReset();
	while (useToastStore.getState().queue.length) dismissToast();
});

afterEach(() => {
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('openExternalUrl', () => {
	it('accepts only http(s) and mailto', () => {
		expect(isExternalUrl('https://ikenga.dev')).toBe(true);
		expect(isExternalUrl('http://localhost:1/x')).toBe(true);
		expect(isExternalUrl('mailto:a@b.co')).toBe(true);
		expect(isExternalUrl('/home/u/file.txt')).toBe(false);
		expect(isExternalUrl('file:///home/u/file.txt')).toBe(false);
		expect(isExternalUrl('javascript:alert(1)')).toBe(false);
		expect(isExternalUrl('C:\\Users\\u')).toBe(false);
	});

	it('rejects a server path instead of window.open-ing it (browser)', async () => {
		host.browser = true;
		const open = vi.spyOn(window, 'open').mockReturnValue(null);
		await expect(openExternalUrl('/home/u/report.pdf')).rejects.toThrow(/Only http\(s\)/);
		expect(open).not.toHaveBeenCalled();
	});

	it('opens a web address in a new tab (browser)', async () => {
		host.browser = true;
		const open = vi.spyOn(window, 'open').mockReturnValue(null);
		await openExternalUrl('https://ikenga.dev');
		expect(open).toHaveBeenCalledWith('https://ikenga.dev', '_blank', 'noopener,noreferrer');
	});
});

describe('openLocalPath', () => {
	it('hides folder actions in a browser, keeps them on desktop', () => {
		host.browser = true;
		expect(canOpenLocalPath('folder')).toBe(false);
		expect(canOpenLocalPath('file')).toBe(true);
		host.browser = false;
		expect(canOpenLocalPath('folder')).toBe(true);
	});

	it('downloads the file through fs_read in a browser and never window.opens', async () => {
		host.browser = true;
		host.invoke.mockResolvedValue({ bytes: [104, 105], mime: 'text/plain' });
		const open = vi.spyOn(window, 'open').mockReturnValue(null);
		const create = vi.fn(() => 'blob:x');
		vi.stubGlobal('URL', Object.assign(URL, { createObjectURL: create, revokeObjectURL: vi.fn() }));
		const clicked: HTMLAnchorElement[] = [];
		vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (
			this: HTMLAnchorElement
		) {
			clicked.push(this);
		});

		await openLocalPath('/srv/data/notes/report.txt', { kind: 'file' });

		expect(host.invoke).toHaveBeenCalledWith('fs_read', { path: '/srv/data/notes/report.txt' });
		expect(create).toHaveBeenCalled();
		expect(clicked).toHaveLength(1);
		expect(clicked[0]?.download).toBe('report.txt');
		expect(open).not.toHaveBeenCalled();
	});

	it('rejects a folder in a browser', async () => {
		host.browser = true;
		await expect(openLocalPath('/srv/data', { kind: 'folder' })).rejects.toThrow(/Folders/);
		expect(host.invoke).not.toHaveBeenCalled();
	});
});

describe('notifications', () => {
	it('reports unsupported when the Notification API is missing (iOS Safari)', async () => {
		host.browser = true;
		vi.stubGlobal('Notification', undefined);
		expect(browserNotificationPermission()).toBe('unsupported');
		expect(await isNotificationPermissionGranted()).toBe(false);
		expect(await requestNotificationPermission()).toBe('denied');
	});

	it('falls back to a toast when the API is missing', async () => {
		host.browser = true;
		vi.stubGlobal('Notification', undefined);
		await sendNotification({ title: 'Build done', body: 'ok' });
		expect(toasts()).toEqual(['Build done — ok']);
	});

	it('falls back to a toast when the constructor throws (Android Chrome)', async () => {
		host.browser = true;
		class Illegal {
			static permission = 'granted';
			constructor() {
				throw new TypeError('Illegal constructor');
			}
		}
		vi.stubGlobal('Notification', Illegal);
		await sendNotification({ title: 'Ping' });
		expect(toasts()).toEqual(['Ping']);
	});

	it('falls back to a toast when permission is not granted', async () => {
		host.browser = true;
		const ctor = vi.fn();
		vi.stubGlobal('Notification', Object.assign(ctor, { permission: 'default' }));
		await sendNotification('Hello');
		expect(ctor).not.toHaveBeenCalled();
		expect(toasts()).toEqual(['Hello']);
	});

	it('raises a real notification when granted', async () => {
		host.browser = true;
		const ctor = vi.fn();
		vi.stubGlobal('Notification', Object.assign(ctor, { permission: 'granted' }));
		await sendNotification({ title: 'T', body: 'B' });
		expect(ctor).toHaveBeenCalledWith('T', { body: 'B', icon: undefined });
		expect(toasts()).toEqual([]);
	});

	it('never prompts outside a user gesture', async () => {
		host.browser = true;
		const request = vi.fn(async () => 'granted');
		vi.stubGlobal('Notification', { permission: 'default', requestPermission: request });
		vi.stubGlobal('navigator', { userActivation: { isActive: false } });
		expect(await requestNotificationPermission()).toBe('default');
		expect(request).not.toHaveBeenCalled();
	});

	it('prompts from a click (active user activation)', async () => {
		host.browser = true;
		const request = vi.fn(async () => 'granted');
		vi.stubGlobal('Notification', { permission: 'default', requestPermission: request });
		vi.stubGlobal('navigator', { userActivation: { isActive: true } });
		expect(await requestNotificationPermission()).toBe('granted');
		expect(request).toHaveBeenCalledOnce();
	});
});
