import { describe, expect, it } from 'vitest';
import type { WslHealth } from '@/lib/tauri-cmd';
import { primaryFixForState, wslHealthCopy, wslHealthStateLabel } from './copy';

function health(over: Partial<WslHealth>): WslHealth {
	return {
		state: 'ok',
		distro: 'Ubuntu',
		detail: '',
		mirroredFailure: null,
		networkingMode: null,
		checkedAt: 1,
		...over,
	};
}

// The owner's machine on 2026-10-07 (live probe, WP-2 backend report).
const MIRRORED_BROKEN = health({
	state: 'no_route',
	distro: null,
	detail:
		"WSL has no network: only the loopback interface is up. WSL's mirrored networking failed to start (0x8007054f) and fell back to no network.",
	mirroredFailure: { at: 1791353662684, errorCode: '0x8007054f' },
	networkingMode: 'mirrored',
});

describe('wslHealthCopy', () => {
	it('says nothing for ok or not_installed', () => {
		expect(wslHealthCopy(health({ state: 'ok' }))).toBeNull();
		expect(wslHealthCopy(health({ state: 'not_installed' }))).toBeNull();
	});

	it('names the failed mirrored setup and its HRESULT for no_route', () => {
		const c = wslHealthCopy(MIRRORED_BROKEN);
		expect(c?.title).toBe(
			"WSL started without a network connection — Windows couldn't set up mirrored networking (0x8007054f)"
		);
		expect(c?.tone).toBe('danger');
		expect(c?.primary?.action).toBe('restart_networking');
		expect(c?.primary?.label).toBe('Restart WSL networking (needs admin)');
		expect(c?.primary?.disruptive).toBe(true);
		expect(c?.secondary.map((s) => s.action)).toEqual(['switch_to_nat']);
		expect(c?.body).toContain('the default distro');
	});

	it('leads with NAT once a restart already failed this episode (D-6)', () => {
		const c = wslHealthCopy(MIRRORED_BROKEN, { restartTried: true });
		expect(c?.primary?.action).toBe('switch_to_nat');
		expect(c?.primary?.label).toBe('Switch to NAT…');
		expect(c?.secondary.map((s) => s.action)).toEqual(['restart_networking']);
	});

	it('offers no NAT switch when WSL is not in mirrored mode', () => {
		const c = wslHealthCopy(health({ state: 'no_route', networkingMode: 'nat' }));
		expect(c?.title).toBe('WSL started without a network connection');
		expect(c?.secondary).toEqual([]);
		// Still no NAT lead, even after a restart.
		expect(
			wslHealthCopy(health({ state: 'no_route' }), { restartTried: true })?.primary?.action
		).toBe('restart_networking');
	});

	it('mirroredFailure without an error code still names the cause', () => {
		const c = wslHealthCopy(
			health({ state: 'no_route', mirroredFailure: { at: 1, errorCode: null } })
		);
		expect(c?.title).toBe(
			"WSL started without a network connection — Windows couldn't set up mirrored networking"
		);
	});

	it('dns_only → Repair DNS, non-disruptive', () => {
		const c = wslHealthCopy(health({ state: 'dns_only' }));
		expect(c?.tone).toBe('warning');
		expect(c?.primary).toEqual({ action: 'repair_dns', label: 'Repair DNS', disruptive: false });
		expect(c?.body).toContain('Ubuntu');
	});

	it('host_offline offers no fix', () => {
		const c = wslHealthCopy(health({ state: 'host_offline' }));
		expect(c?.title).toBe('Your computer is offline');
		expect(c?.primary).toBeNull();
		expect(c?.secondary).toEqual([]);
	});

	it('wsl_down carries the probe detail and offers a restart', () => {
		const c = wslHealthCopy(health({ state: 'wsl_down', detail: 'wsl.exe timed out.' }));
		expect(c?.title).toBe("WSL isn't starting");
		expect(c?.body.startsWith('wsl.exe timed out.')).toBe(true);
		expect(c?.primary?.action).toBe('restart_networking');
	});
});

describe('primaryFixForState', () => {
	it('maps notification states to their fix', () => {
		expect(primaryFixForState('dns_only')?.action).toBe('repair_dns');
		expect(primaryFixForState('no_route')?.action).toBe('restart_networking');
		expect(primaryFixForState('wsl_down')?.action).toBe('restart_networking');
		expect(primaryFixForState('host_offline')).toBeNull();
		expect(primaryFixForState('ok')).toBeNull();
	});
});

describe('wslHealthStateLabel', () => {
	it('labels every state', () => {
		expect(wslHealthStateLabel('ok')).toBe('Online');
		expect(wslHealthStateLabel('no_route')).toBe('No network');
		expect(wslHealthStateLabel('dns_only')).toBe('DNS failing');
	});
});
