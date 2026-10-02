import { describe, expect, it } from 'vitest';

import type { AccessStatus, DeviceView } from '@/lib/access/client';

import {
	approverNames,
	routingDisabledReason,
	routingNote,
	thisDeviceDisabledReason,
} from './routing-policy';

const dev = (
	deviceId: string,
	name: string,
	tier: DeviceView['tier'],
	kind: DeviceView['kind'] = 'paired'
): DeviceView => ({
	deviceId,
	kind,
	name,
	platform: null,
	tier,
	pairedAt: 0,
	lastSeenAt: null,
	lastSeenAddr: null,
	liveSockets: 0,
	thisDevice: false,
});

const DEVICES = [
	dev('host', 'ned-desktop', 'full', 'host'),
	dev('pixel', 'Pixel 9 · Chrome', 'dispatch'),
	dev('mac', 'ned-macbook', 'approve'),
];

interface Over {
	adminStrength?: boolean;
	store?: AccessStatus['store'];
	credTier?: DeviceView['tier'];
	via?: 'operator' | 'device' | 'session';
	deviceId?: string | null;
}

function status(over: Over = {}): AccessStatus {
	return {
		tier: 't0',
		store: 'ok',
		principal: { principalId: 'p', username: 'ned', isAdmin: false },
		credential: {
			via: over.via ?? 'operator',
			deviceId: over.deviceId === undefined ? 'host' : over.deviceId,
			tier: over.credTier ?? 'full',
		},
		caps: ['files', 'sessions', 'dispatch', 'approve', 'install', 'settings', 'secrets'],
		adminStrength: over.adminStrength ?? true,
		publicUrl: null,
		sharingEnabled: false,
		share: null,
		...(over.store ? { store: over.store } : {}),
	};
}

describe('routing policy (G-ACCESS §5.1, D-05 approveNote)', () => {
	it('names the paired devices that may approve under "any paired device"', () => {
		// D-05: the host is not a paired device, so only ned-macbook is named.
		expect(approverNames(DEVICES)).toEqual(['ned-macbook']);
		expect(routingNote({ mode: 'any_approve', deviceId: null }, DEVICES, 'host')).toEqual({
			kind: 'any',
			approvers: ['ned-macbook'],
		});
		// Only the host: nobody to name.
		expect(approverNames([DEVICES[0]])).toEqual([]);
	});

	it('"this device only" set here names this device; set elsewhere names that one', () => {
		expect(
			routingNote(
				{ mode: 'this_device', deviceId: 'host', deviceName: 'ned-desktop' },
				DEVICES,
				'host'
			)
		).toEqual({ kind: 'here', device: 'ned-desktop' });
		expect(routingNote({ mode: 'this_device', deviceId: 'mac' }, DEVICES, 'host')).toEqual({
			kind: 'elsewhere',
			device: 'ned-macbook',
		});
		expect(
			routingNote({ mode: 'this_device', deviceId: 'gone', deviceName: null }, DEVICES, 'host')
		).toEqual({
			kind: 'elsewhere',
			device: null,
		});
	});

	it('only an admin-strength credential with a store may change it', () => {
		expect(routingDisabledReason(status())).toBeNull();
		expect(
			routingDisabledReason(status({ adminStrength: false, credTier: 'approve', via: 'device' }))
		).toMatch(/Dispatch \+ approve/);
		expect(routingDisabledReason(status({ store: 'none' }))).toMatch(/No access store/);
		expect(routingDisabledReason(status({ store: 'degraded' }))).toMatch(/paused/);
	});

	it('"this device only" needs a device that can approve', () => {
		expect(thisDeviceDisabledReason(status())).toBeNull();
		expect(
			thisDeviceDisabledReason(status({ credTier: 'dispatch', via: 'device', deviceId: 'pixel' }))
		).toMatch(/can't approve/);
		expect(thisDeviceDisabledReason(status({ via: 'session', deviceId: null }))).toMatch(
			/not a paired device/
		);
	});
});
