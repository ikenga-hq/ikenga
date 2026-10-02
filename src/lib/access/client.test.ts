// G-ACCESS client (access-schema §1.5, §9.1): wrapper arg shapes, the
// generated tables, and the disable-with-a-reason helper.
import { beforeEach, describe, expect, it, vi } from 'vitest';

const invoke = vi.fn(async (..._args: unknown[]) => ({}) as unknown);

vi.mock('../transport', () => ({
	getTransport: () => ({ invoke, listen: vi.fn() }),
}));

import {
	ACCESS_SCHEMA_VERSION,
	CAP_LABELS,
	CAPS,
	NEVER_GRANTABLE,
	ROLE_DEFAULT_CAPS,
	ROLES,
	TIER_CAPS,
	TIER_LABELS,
	TIERS,
} from './caps.gen';
import {
	type AccessStatus,
	accessAuditExport,
	accessDeviceRevoke,
	accessDeviceSetTier,
	accessErrorCode,
	accessPairBegin,
	accessPairDecide,
	accessRoutingSet,
	accessStatus,
	disabledReason,
	missingCaps,
	permissionDecide,
	requirementOf,
} from './client';
import { RPC_REQUIREMENTS } from './rpc-requirements.gen';

function status(over: Partial<AccessStatus> = {}): AccessStatus {
	return {
		tier: 't0',
		store: 'ok',
		principal: { principalId: 'p', username: 'ned', isAdmin: false },
		credential: { via: 'device', deviceId: 'd', tier: 'dispatch' },
		caps: [...TIER_CAPS.dispatch],
		adminStrength: false,
		publicUrl: null,
		sharingEnabled: false,
		share: null,
		...over,
	};
}

describe('generated capability tables (caps.gen.ts)', () => {
	it('is schema version 1 with the seven D-05 rows', () => {
		expect(ACCESS_SCHEMA_VERSION).toBe(1);
		expect(CAPS).toEqual([
			'files',
			'sessions',
			'dispatch',
			'approve',
			'install',
			'settings',
			'secrets',
		]);
		expect(Object.keys(CAP_LABELS)).toEqual([...CAPS]);
	});

	it('orders tiers as supersets ending in all seven', () => {
		expect(TIERS).toEqual(['view', 'dispatch', 'approve', 'full']);
		for (let i = 1; i < TIERS.length; i++) {
			const lower = TIER_CAPS[TIERS[i - 1]];
			const upper = TIER_CAPS[TIERS[i]];
			expect(upper.length).toBeGreaterThan(lower.length);
			for (const c of lower) expect(upper).toContain(c);
		}
		expect(TIER_CAPS.full).toEqual([...CAPS]);
		expect(TIER_CAPS.dispatch).not.toContain('approve');
		expect(Object.keys(TIER_LABELS)).toEqual([...TIERS]);
	});

	it('never grants secrets to a non-Owner (§4.1)', () => {
		expect(ROLES).toEqual(['owner', 'operator', 'reviewer', 'guest']);
		expect(ROLE_DEFAULT_CAPS.owner).toEqual([...CAPS]);
		for (const role of ['operator', 'reviewer', 'guest'] as const) {
			expect(ROLE_DEFAULT_CAPS[role]).not.toContain('secrets');
			expect(NEVER_GRANTABLE[role]).toEqual(['secrets']);
		}
		expect(ROLE_DEFAULT_CAPS.guest).toEqual([]);
	});
});

describe('generated route requirements (rpc-requirements.gen.ts)', () => {
	it('maps every access command, with P-2 and P-3 rows as specified', () => {
		for (const cmd of ['access_status', 'access_devices_list', 'access_pair_begin']) {
			expect(RPC_REQUIREMENTS[cmd]).toEqual({ caps: [], class: 'access' });
		}
		expect(RPC_REQUIREMENTS.fs_write).toEqual({ caps: ['files', 'dispatch'], class: 'shared' });
		expect(RPC_REQUIREMENTS.pty_spawn.class).toBe('owner');
		expect(RPC_REQUIREMENTS.permission_decide).toEqual({ caps: ['approve'], class: 'shared' });
		expect(RPC_REQUIREMENTS.permission_relay_put.class).toBe('operator');
		expect(RPC_REQUIREMENTS.share_project_info.class).toBe('internal');
	});

	it('treats an unmapped command as needing everything (§1.6 rule 2)', () => {
		expect(requirementOf('no_such_arm')).toEqual({ caps: CAPS, class: 'owner' });
	});
});

describe('wrapper arg shapes', () => {
	beforeEach(() => invoke.mockClear());

	it('sends the §9.1 names and camelCase args', async () => {
		await accessStatus();
		await accessDeviceSetTier('dev-1', 'view');
		await accessDeviceRevoke('dev-1');
		await accessPairBegin();
		await accessPairBegin('http://100.94.12.7:4000');
		await accessPairDecide('pair-1', 'allow', 'dispatch');
		await accessRoutingSet('this_device', 'dev-1');
		await permissionDecide(42, 'deny');
		await accessAuditExport({ category: 'pairing' });
		expect(invoke.mock.calls).toEqual([
			['access_status', {}],
			['access_device_set_tier', { deviceId: 'dev-1', tier: 'view' }],
			['access_device_revoke', { deviceId: 'dev-1' }],
			['access_pair_begin', {}],
			['access_pair_begin', { publicBase: 'http://100.94.12.7:4000' }],
			['access_pair_decide', { pairingId: 'pair-1', decision: 'allow', tier: 'dispatch' }],
			['access_routing_set', { mode: 'this_device', deviceId: 'dev-1' }],
			['permission_decide', { notificationId: 42, decision: 'deny' }],
			['access_audit_export', { filter: { category: 'pairing' } }],
		]);
	});
});

describe('disabling controls with a reason', () => {
	it('names the missing cap and the device tier', () => {
		expect(missingCaps('pa_actions_commit', TIER_CAPS.dispatch)).toEqual(['approve']);
		expect(disabledReason('pa_actions_commit', status())).toBe(
			'Needs approve — this device is View + dispatch'
		);
		expect(disabledReason('fs_read', status())).toBeNull();
	});

	it('refuses operator-class, internal and owner-in-share commands', () => {
		const full = status({
			caps: [...CAPS],
			credential: { via: 'device', deviceId: 'd', tier: 'full' },
		});
		expect(disabledReason('permission_relay_put', full)).toBe('Only this computer can do that');
		expect(disabledReason('share_project_info', full)).toBe('Not available here');
		const shared = status({
			caps: [...CAPS],
			share: {
				projectKey: 'o/p',
				projectName: 'p',
				ownerUsername: 'ada',
				role: 'operator',
				scope: 'project',
			},
		});
		expect(disabledReason('pty_spawn', shared)).toBe('Not available in a shared project');
		const operator = status({
			caps: [...CAPS],
			credential: { via: 'operator', deviceId: 'h', tier: 'full' },
		});
		expect(disabledReason('permission_relay_put', operator)).toBeNull();
	});

	it('parses the closed error-code set', () => {
		expect(accessErrorCode('forbidden: missing=dispatch')).toBe('forbidden');
		expect(accessErrorCode(new Error('store_unavailable: no daemon'))).toBe('store_unavailable');
		expect(accessErrorCode('Command x not implemented')).toBeNull();
	});
});
