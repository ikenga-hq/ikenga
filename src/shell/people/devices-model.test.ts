import { describe, expect, it, vi } from 'vitest';
import type { DeviceView } from '@/lib/access/client';
import type { DaemonInfo } from '@/lib/tauri-cmd';

import {
	buildDevicesView,
	classifyHost,
	deviceSubLine,
	expiresIn,
	maskToken,
	pairedCount,
	pairPublicBase,
	probeDaemon,
	relativeTime,
} from './devices-model';

const PERSISTENT: DaemonInfo = {
	available: true,
	host: '127.0.0.1',
	port: 4000,
	token: 'abcdef0123456789deadbeef',
	httpUrl: 'http://127.0.0.1:4000',
	wsUrl: 'ws://127.0.0.1:4000',
	pid: 4242,
	mode: 'persistent',
};

describe('classifyHost', () => {
	it.each([
		['127.0.0.1', 'loopback'],
		['localhost', 'loopback'],
		['[::1]', 'loopback'],
		['0.0.0.0', 'all-interfaces'],
		['::', 'all-interfaces'],
		['100.94.12.31', 'tailnet'],
		['100.64.0.1', 'tailnet'],
		['ikenga.tailnet-demo.ts.net', 'tailnet'],
		['fd7a:115c:a1e0::1', 'tailnet'],
		['100.63.0.1', 'public'],
		['192.168.1.20', 'lan'],
		['10.0.0.5', 'lan'],
		['172.20.1.1', 'lan'],
		['ned-desktop.local', 'lan'],
		['8.8.8.8', 'public'],
		['example.com', 'unknown'],
		['', 'unknown'],
	] as const)('%s → %s', (host, expected) => {
		expect(classifyHost(host)).toBe(expected);
	});
});

describe('maskToken', () => {
	it('never shows more than the last four characters', () => {
		const masked = maskToken('abcdef0123456789deadbeef');
		expect(masked).toBe('••••••••beef');
		expect(masked).not.toContain('abcdef');
	});

	it('hides short tokens entirely and shows nothing for none', () => {
		expect(maskToken('short')).toBe('••••••••');
		expect(maskToken('')).toBe('');
		expect(maskToken(null)).toBe('');
	});
});

describe('buildDevicesView', () => {
	it('reports a live loopback daemon with its token masked', () => {
		const view = buildDevicesView(PERSISTENT, null, 'up');
		expect(view).toMatchObject({
			source: 'desktop',
			run: 'running',
			address: 'http://127.0.0.1:4000',
			exposure: 'loopback',
			tailnetAddress: null,
			tokenPresent: true,
			tokenMasked: '••••••••beef',
			pid: 4242,
		});
		expect(JSON.stringify(view)).not.toContain(PERSISTENT.token);
	});

	it('names the tailnet address when bound to one', () => {
		const view = buildDevicesView(
			{ ...PERSISTENT, host: '100.94.12.31', httpUrl: 'http://100.94.12.31:4000' },
			null,
			'up'
		);
		expect(view.exposure).toBe('tailnet');
		expect(view.tailnetAddress).toBe('100.94.12.31');
	});

	it('says stopped when the launch-time daemon no longer answers', () => {
		const view = buildDevicesView(PERSISTENT, null, 'down');
		expect(view.run).toBe('stopped');
		expect(view.runNote).toMatch(/stopped answering/);
	});

	it('says stopped, and exposes nothing, in ephemeral mode', () => {
		const view = buildDevicesView(
			{ ...PERSISTENT, available: false, mode: 'ephemeral', token: '' },
			null,
			'skipped'
		);
		expect(view.run).toBe('stopped');
		expect(view.address).toBeNull();
		expect(view.tokenPresent).toBe(false);
	});

	it('is unknown when the window cannot read the daemon', () => {
		expect(buildDevicesView(null, null, 'skipped').run).toBe('unknown');
	});

	it('uses the page origin in a remote browser session', () => {
		const view = buildDevicesView(
			null,
			{
				origin: 'https://ikenga.tailnet-demo.ts.net',
				hostname: 'ikenga.tailnet-demo.ts.net',
				hasToken: true,
			},
			'up'
		);
		expect(view).toMatchObject({ source: 'remote', run: 'running', exposure: 'tailnet' });
	});
});

describe('probeDaemon', () => {
	it('is up when anything answers, even opaquely', async () => {
		const fetchImpl = vi.fn().mockResolvedValue(new Response(null, { status: 200 }));
		await expect(probeDaemon('http://127.0.0.1:4000/', 100, fetchImpl)).resolves.toBe('up');
		expect(fetchImpl).toHaveBeenCalledWith(
			'http://127.0.0.1:4000/api/health',
			expect.objectContaining({ mode: 'no-cors' })
		);
	});

	it('is down on a network error', async () => {
		const fetchImpl = vi.fn().mockRejectedValue(new TypeError('Failed to fetch'));
		await expect(probeDaemon('http://127.0.0.1:4000', 100, fetchImpl)).resolves.toBe('down');
	});
});

// ── WP-74b ──────────────────────────────────────────────────────────────────

describe('pairPublicBase (G-ACCESS §3.3 rule 2)', () => {
	it('is the daemon address for a tailnet or LAN bind, nothing for loopback', () => {
		const tail = buildDevicesView(
			{ ...PERSISTENT, host: '100.94.12.7', httpUrl: 'http://100.94.12.7:4000/' },
			null,
			'up'
		);
		expect(pairPublicBase(tail)).toBe('http://100.94.12.7:4000');
		const lan = buildDevicesView(
			{ ...PERSISTENT, host: '192.168.1.20', httpUrl: 'http://192.168.1.20:4000' },
			null,
			'up'
		);
		expect(pairPublicBase(lan)).toBe('http://192.168.1.20:4000');
		expect(pairPublicBase(buildDevicesView(PERSISTENT, null, 'up'))).toBeUndefined();
		const all = buildDevicesView({ ...PERSISTENT, host: '0.0.0.0' }, null, 'up');
		expect(pairPublicBase(all)).toBeUndefined();
		const down = buildDevicesView(
			{ ...PERSISTENT, host: '100.94.12.7', httpUrl: 'http://100.94.12.7:4000' },
			null,
			'down'
		);
		expect(pairPublicBase(down)).toBeUndefined();
		const remote = buildDevicesView(
			null,
			{ origin: 'http://100.94.12.7:4000', hostname: '100.94.12.7', hasToken: true },
			'up'
		);
		expect(pairPublicBase(remote)).toBeUndefined();
	});
});

describe('table formatting', () => {
	it('relativeTime / expiresIn', () => {
		const now = 1_000_000_000;
		expect(relativeTime(null, now)).toBe('—');
		expect(relativeTime(now - 2_000, now)).toBe('now');
		expect(relativeTime(now - 12_000, now)).toBe('12 s ago');
		expect(relativeTime(now - 4 * 60_000, now)).toBe('4 min ago');
		expect(relativeTime(now - 2 * 3_600_000, now)).toBe('2 h ago');
		expect(relativeTime(now - 3 * 86_400_000, now)).toBe('3 d ago');
		expect(expiresIn(now + 598_000, now)).toBe('9:58');
		expect(expiresIn(now - 1, now)).toBe('0:00');
	});

	it('deviceSubLine / pairedCount', () => {
		const host: DeviceView = {
			deviceId: 'h',
			kind: 'host',
			name: 'ned-desktop',
			platform: 'linux',
			tier: 'full',
			pairedAt: 1,
			lastSeenAt: null,
			lastSeenAddr: null,
			liveSockets: 0,
			thisDevice: true,
		};
		const phone: DeviceView = {
			...host,
			deviceId: 'p',
			kind: 'paired',
			name: 'Pixel 9 · Chrome',
			platform: 'android',
			tier: 'dispatch',
			thisDevice: false,
		};
		expect(deviceSubLine(host)).toBe('Linux · this device');
		expect(deviceSubLine(phone)).toMatch(/^Android · paired /);
		expect(pairedCount([host, phone])).toBe(1);
	});
});
