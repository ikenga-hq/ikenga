// Trust-review modal — wrapper + formatter tests. The shell vitest config
// has no DOM env, so we exercise the typed Tauri wrappers and the pure
// JSON formatter rather than rendering the React tree. Visual layout is
// verified during live boot smoke (deferred — see plan status log).

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { _formatJson } from './trust-review-modal';

vi.mock('@tauri-apps/api/core', () => ({
	invoke: vi.fn(),
}));

import { invoke } from '@tauri-apps/api/core';
import {
	pkgTrustApprove,
	pkgTrustListPending,
	pkgTrustReject,
	type PkgTrustReview,
} from '@/lib/tauri-cmd';

const invokeMock = vi.mocked(invoke);

beforeEach(() => {
	(globalThis as any).__TAURI_INTERNALS__ = {};
});

afterEach(() => {
	invokeMock.mockReset();
	delete (globalThis as any).__TAURI_INTERNALS__;
});

describe('_formatJson', () => {
	it('pretty-prints valid JSON with 2-space indent', () => {
		const out = _formatJson('{"a":1,"b":[2,3]}');
		expect(out).toContain('"a": 1');
		expect(out).toContain('  ');
	});

	it('returns the raw string when input is not valid JSON', () => {
		expect(_formatJson('not json {')).toBe('not json {');
	});
});

describe('trust-review tauri-cmd wrappers', () => {
	it('pkgTrustListPending calls pkg_trust_list_pending and returns the list', async () => {
		const sample: PkgTrustReview[] = [
			{
				pkg_id: 'com.test.one',
				manifest_version: '0.2.0',
				old_capabilities: '{"capabilities":null,"permissions":{}}',
				new_capabilities: '{"capabilities":null,"permissions":{"net":["x"]}}',
				prior_approved_at_ms: 1700000000000,
			},
		];
		invokeMock.mockResolvedValueOnce(sample);
		const out = await pkgTrustListPending();
		expect(invokeMock).toHaveBeenCalledWith('pkg_trust_list_pending');
		expect(out).toEqual(sample);
	});

	it('pkgTrustApprove sends pkg_trust_approve with the pkgId arg', async () => {
		invokeMock.mockResolvedValueOnce(undefined);
		await pkgTrustApprove('com.test.one');
		expect(invokeMock).toHaveBeenCalledWith('pkg_trust_approve', {
			pkgId: 'com.test.one',
		});
	});

	it('pkgTrustReject sends pkg_trust_reject with the pkgId arg', async () => {
		invokeMock.mockResolvedValueOnce(undefined);
		await pkgTrustReject('com.test.two');
		expect(invokeMock).toHaveBeenCalledWith('pkg_trust_reject', {
			pkgId: 'com.test.two',
		});
	});

	it('pkgTrustApprove propagates a Rust-side error', async () => {
		invokeMock.mockRejectedValueOnce('uninstall failed: locked');
		await expect(pkgTrustApprove('com.test.one')).rejects.toBe('uninstall failed: locked');
	});
});
