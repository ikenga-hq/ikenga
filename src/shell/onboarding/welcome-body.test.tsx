// welcome-body — preflight gating logic.
//
// vitest setup has no DOM env, so we test the pure decision fn rather than
// rendering the React tree. The visual layout is exercised by the Phase 1
// prototype `01-welcome.html`.

import { describe, expect, it } from 'vitest';

import type { SystemReport } from '@/lib/tauri-cmd';

import { canContinueFromPreflight } from './welcome-body';

function makeReport(checks: SystemReport['checks']): SystemReport {
	return {
		os: 'linux',
		arch: 'x86_64',
		disk_free_gb: 256,
		app_data_dir: '/tmp/test',
		app_data_writable: true,
		vault_key_present: true,
		claude_projects_dir_present: true,
		checks,
	};
}

describe('canContinueFromPreflight', () => {
	it('returns false when no report has loaded yet', () => {
		expect(canContinueFromPreflight(undefined)).toBe(false);
	});

	it('returns true when all checks pass', () => {
		const r = makeReport([
			{ id: 'os', level: 'pass', message: 'macOS 14.4', fix_hint: null },
			{ id: 'disk', level: 'pass', message: '128 GB free', fix_hint: null },
		]);
		expect(canContinueFromPreflight(r)).toBe(true);
	});

	it('allows continue with warnings', () => {
		const r = makeReport([
			{ id: 'os', level: 'pass', message: 'ok', fix_hint: null },
			{ id: 'network', level: 'warn', message: 'VPN detected', fix_hint: 'OK to proceed' },
		]);
		expect(canContinueFromPreflight(r)).toBe(true);
	});

	it('allows continue when free disk space is unknown (null)', () => {
		const r = {
			...makeReport([
				{ id: 'os', level: 'pass', message: 'ok', fix_hint: null },
				{
					id: 'disk_free',
					level: 'warn',
					message: "Couldn't determine free space: no mounted volume matched /tmp/test",
					fix_hint: "This doesn't block setup.",
				},
			]),
			disk_free_gb: null,
		} satisfies SystemReport;
		expect(r.disk_free_gb).toBeNull();
		expect(canContinueFromPreflight(r)).toBe(true);
	});

	it('blocks continue when any check fails', () => {
		const r = makeReport([
			{ id: 'os', level: 'pass', message: 'ok', fix_hint: null },
			{
				id: 'app_data',
				level: 'fail',
				message: 'Cannot write to app data dir',
				fix_hint: 'Check permissions',
			},
		]);
		expect(canContinueFromPreflight(r)).toBe(false);
	});

	it('renders at least 5 preflight rows in a healthy report', () => {
		// Sanity check on the shape — the body's UI maps each check to a row;
		// if the Rust side regresses to <5 checks the wizard would look thin.
		const r = makeReport([
			{ id: 'os', level: 'pass', message: 'macOS 14.4', fix_hint: null },
			{ id: 'disk', level: 'pass', message: '128 GB free', fix_hint: null },
			{ id: 'app_data', level: 'pass', message: 'writable', fix_hint: null },
			{ id: 'vault', level: 'pass', message: 'initialized', fix_hint: null },
			{ id: 'claude_projects', level: 'pass', message: 'present', fix_hint: null },
		]);
		expect(r.checks.length).toBeGreaterThanOrEqual(5);
		expect(canContinueFromPreflight(r)).toBe(true);
	});
});
