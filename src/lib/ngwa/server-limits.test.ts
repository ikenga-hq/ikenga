import { describe, expect, it } from 'vitest';
import { isServerLimit, serverLimits, serverLimitsLine, sourceLabel } from './server-limits';

const ENGINE =
	'engine asset placements are not available on this server: the headless daemon runs no engine_assets registry';
const KERNEL =
	'pkg runtime state is not available on this server: the headless daemon runs no sidecar supervisor';
const TRUST =
	'trust evaluation is not available on this server: the headless daemon keeps no trust store';

const src = (source: string, error: string | null, unavailable?: boolean) => ({
	source,
	error,
	unavailable,
});

describe('server limits (unavailable by design vs failed)', () => {
	it('keys on the typed field, never on the prose', () => {
		// Same words, but the daemon did not mark it: a failure, not a limit.
		expect(isServerLimit({ unavailable: false })).toBe(false);
		expect(isServerLimit({})).toBe(false);
		expect(isServerLimit(undefined)).toBe(false);
		expect(isServerLimit({ unavailable: true })).toBe(true);
		const limits = serverLimits([src('usage', 'boom: not available on this server')]);
		expect(limits).toEqual([]);
	});

	it('orders by notice order and labels in plain words', () => {
		const limits = serverLimits([
			src('engine_assets', ENGINE, true),
			src('trust', TRUST, true),
			src('kernel', KERNEL, true),
			src('oba', 'permission denied', false),
		]);
		expect(limits.map((l) => l.source)).toEqual(['kernel', 'trust', 'engine_assets']);
		expect(serverLimitsLine(limits)).toBe(
			'On this server: package runtime, trust and engine placement are desktop-only'
		);
	});

	it('reads singular for one limit and plural for two', () => {
		expect(serverLimitsLine(serverLimits([src('trust', TRUST, true)]))).toBe(
			'On this server: trust is desktop-only'
		);
		expect(
			serverLimitsLine(serverLimits([src('kernel', KERNEL, true), src('trust', TRUST, true)]))
		).toBe('On this server: package runtime and trust are desktop-only');
		expect(serverLimitsLine([])).toBe('');
	});

	it('gives each its one-line reason without repeating the lead-in', () => {
		const [k] = serverLimits([src('kernel', KERNEL, true)]);
		expect(k?.reason).toBe('The headless daemon runs no sidecar supervisor');
		const [odd] = serverLimits([src('trust', null, true)]);
		expect(odd?.reason).toBe('not run on this server');
		expect(sourceLabel('some_new_source')).toBe('some new source');
	});
});
