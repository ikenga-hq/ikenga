// G-ACCESS (WP-74a): the generated vocabulary and the §9.1 wrappers.
import { beforeEach, describe, expect, it, vi } from 'vitest';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn(async () => ({})) }));

vi.mock('@/lib/tauri-cmd', () => ({ invoke }));

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
	accessDeviceSetTier,
	accessPairBegin,
	disabledReason,
	missingCaps,
	parseAccessError,
	permissionDecide,
	requirementFor,
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

describe('caps.gen (G-ACCESS §1)', () => {
	it('has the seven rows, four tiers and four roles', () => {
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
		expect(TIERS).toEqual(['view', 'dispatch', 'approve', 'full']);
		expect(ROLES).toEqual(['owner', 'operator', 'reviewer', 'guest']);
		for (const cap of CAPS) expect(CAP_LABELS[cap].label).toBeTruthy();
		for (const tier of TIERS) expect(TIER_LABELS[tier].label).toBeTruthy();
	});

	it('orders tiers as supersets, with full = all seven and no approve below approve', () => {
		for (let i = 1; i < TIERS.length; i++) {
			const lower = TIER_CAPS[TIERS[i - 1]];
			const upper = TIER_CAPS[TIERS[i]];
			for (const c of lower) expect(upper).toContain(c);
			expect(upper.length).toBeGreaterThan(lower.length);
		}
		expect([...TIER_CAPS.full]).toEqual([...CAPS]);
		expect(TIER_CAPS.dispatch).not.toContain('approve');
	});

	it('never grants secrets to a non-Owner role', () => {
		for (const role of ['operator', 'reviewer', 'guest'] as const) {
			expect(NEVER_GRANTABLE[role]).toEqual(['secrets']);
			expect(ROLE_DEFAULT_CAPS[role]).not.toContain('secrets');
		}
		expect([...ROLE_DEFAULT_CAPS.owner]).toEqual([...CAPS]);
	});
});

describe('rpc-requirements.gen (G-ACCESS §1.6)', () => {
	it('names only known caps and classes', () => {
		const classes = new Set(['shared', 'owner', 'operator', 'access', 'internal']);
		const entries = Object.entries(RPC_REQUIREMENTS);
		expect(entries.length).toBeGreaterThan(150);
		for (const [name, req] of entries) {
			expect(classes.has(req.class), name).toBe(true);
			for (const c of req.caps) expect(CAPS, name).toContain(c);
			if (name.startsWith('access_')) expect(req.class, name).toBe('access');
		}
		expect(RPC_REQUIREMENTS.permission_relay_put.class).toBe('operator');
		expect(RPC_REQUIREMENTS.share_project_info.class).toBe('internal');
		expect(RPC_REQUIREMENTS.fs_write.caps).toEqual(['files', 'dispatch']);
	});

	it('treats an unmapped command as owner with all seven (rule 2)', () => {
		expect(requirementFor('no_such_cmd')).toEqual({ caps: [...CAPS], class: 'owner' });
	});

	it('derives disabled reasons for UI copy only', () => {
		expect(missingCaps('pa_actions_commit', TIER_CAPS.dispatch)).toEqual(['approve']);
		expect(disabledReason('pa_actions_commit', status())).toBe(
			'Needs approve — this device is View + dispatch'
		);
		expect(disabledReason('pty_write', status())).toBeNull();
		expect(disabledReason('permission_relay_take', status())).toBe('Only the host can do this');
		expect(
			disabledReason(
				'db_exec',
				status({
					caps: [...CAPS],
					share: {
						projectKey: 'o/p',
						projectName: 'p',
						ownerUsername: 'ada',
						role: 'operator',
						scope: 'project',
					},
				})
			)
		).toBe('Not available in a shared project');
	});
});

describe('client wrappers (G-ACCESS §9.1)', () => {
	beforeEach(() => invoke.mockClear());

	it('send the §9.1 argument names', async () => {
		await accessDeviceSetTier('dev-1', 'approve');
		expect(invoke).toHaveBeenLastCalledWith('access_device_set_tier', {
			deviceId: 'dev-1',
			tier: 'approve',
		});
		await accessPairBegin();
		expect(invoke).toHaveBeenLastCalledWith('access_pair_begin', {});
		await accessPairBegin('http://100.1.2.3:4000');
		expect(invoke).toHaveBeenLastCalledWith('access_pair_begin', {
			publicBase: 'http://100.1.2.3:4000',
		});
		await permissionDecide(7, 'deny');
		expect(invoke).toHaveBeenLastCalledWith('permission_decide', {
			notificationId: 7,
			decision: 'deny',
		});
	});

	it('parses `<code>: <message>` errors', () => {
		expect(parseAccessError('forbidden: missing=dispatch')).toEqual({
			code: 'forbidden',
			message: 'missing=dispatch',
		});
		expect(parseAccessError(new Error('store_unavailable: no store'))).toEqual({
			code: 'store_unavailable',
			message: 'no store',
		});
		expect(parseAccessError('Something else')).toEqual({ code: null, message: 'Something else' });
	});
});
