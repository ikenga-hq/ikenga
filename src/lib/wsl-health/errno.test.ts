import { describe, expect, it, vi } from 'vitest';
import {
	createNetworkErrnoScanner,
	ERRNO_PROBE_DEBOUNCE_MS,
	shouldForceProbeOnErrno,
} from './errno';

const enc = (s: string) => new TextEncoder().encode(s);

describe('createNetworkErrnoScanner', () => {
	it('hits on the errnos that mean WSL has no network', () => {
		for (const line of [
			'OAuth error: getaddrinfo EAI_AGAIN platform.claude.com',
			'connect ENETUNREACH 1.2.3.4:443',
			'curl: (6) Could not resolve host: x (Temporary failure in name resolution)',
			'ping: connect: Network is unreachable',
		]) {
			const hit = vi.fn();
			createNetworkErrnoScanner(hit)(enc(line));
			expect(hit, line).toHaveBeenCalledTimes(1);
		}
	});

	it('ignores ordinary output, including generic timeouts', () => {
		const hit = vi.fn();
		const scan = createNetworkErrnoScanner(hit);
		scan(enc('Request timed out\r\n'));
		scan(enc('connect ECONNREFUSED 127.0.0.1:3000\r\n'));
		scan(enc('EAI_AGAINST nothing\r\n'));
		// A dead / typo'd host on a healthy network — not WSL's problem.
		scan(enc('npm ERR! getaddrinfo ENOTFOUND registry.npmjs.orgg\r\n'));
		expect(hit).not.toHaveBeenCalled();
	});

	it('sees a match split across chunks and through ANSI colour', () => {
		const hit = vi.fn();
		const scan = createNetworkErrnoScanner(hit);
		scan(enc('getaddrinfo \x1b[31mEAI_'));
		scan(enc('AGAIN\x1b[0m platform.claude.com'));
		expect(hit).toHaveBeenCalledTimes(1);
	});

	it('does not re-report the carried tail', () => {
		const hit = vi.fn();
		const scan = createNetworkErrnoScanner(hit);
		scan(enc('EAI_AGAIN'));
		scan(enc(' more output'));
		expect(hit).toHaveBeenCalledTimes(1);
	});
});

describe('shouldForceProbeOnErrno', () => {
	const now = 100_000;
	it('probes when nothing is known or the cache says ok', () => {
		expect(
			shouldForceProbeOnErrno({ cachedState: undefined, lastForcedAt: null, inFlight: false, now })
		).toBe(true);
		expect(
			shouldForceProbeOnErrno({ cachedState: 'ok', lastForcedAt: null, inFlight: false, now })
		).toBe(true);
	});

	it('once per episode: not while a problem is already known', () => {
		expect(
			shouldForceProbeOnErrno({ cachedState: 'no_route', lastForcedAt: null, inFlight: false, now })
		).toBe(false);
	});

	it('debounced, and never alongside a probe in flight', () => {
		expect(
			shouldForceProbeOnErrno({
				cachedState: 'ok',
				lastForcedAt: now - 1_000,
				inFlight: false,
				now,
			})
		).toBe(false);
		expect(
			shouldForceProbeOnErrno({
				cachedState: 'ok',
				lastForcedAt: now - ERRNO_PROBE_DEBOUNCE_MS,
				inFlight: false,
				now,
			})
		).toBe(true);
		expect(
			shouldForceProbeOnErrno({ cachedState: 'ok', lastForcedAt: null, inFlight: true, now })
		).toBe(false);
	});
});
