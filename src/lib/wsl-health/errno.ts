// honest-failure-states WP-2 (D-7) — spot a network errno in a WSL tab's
// output (`getaddrinfo EAI_AGAIN platform.claude.com`, curl's "Temporary
// failure in name resolution", …) so the shell can re-probe WSL at the moment
// the problem shows instead of polling. Pure; unit-tested.
//
// Narrower than the Ngwa install classifier's network rule
// (`src/lib/ngwa/install-errors.ts`): a terminal prints "timed out" and
// ECONNREFUSED for plenty of reasons that say nothing about WSL's network, so
// only the errnos that mean "this machine can't resolve / route" count.

import { stripAnsi } from '@/terminal/pty-output-buffer';

export const NETWORK_ERRNO_RE =
	/\b(EAI_AGAIN|ENOTFOUND|ENETUNREACH)\b|Temporary failure in name resolution|Network is unreachable/i;

/** Characters kept from the previous chunk so a match split across two PTY
 *  reads is still seen. Longer than the longest pattern. */
const CARRY = 64;

/**
 * A `Pty.onData` consumer that calls `onHit` when the output shows a network
 * errno. Decodes UTF-8 across chunk boundaries, strips ANSI, and carries a
 * short tail between chunks. Gating (once per episode, debounce) is the
 * caller's: see {@link shouldForceProbeOnErrno}.
 */
export function createNetworkErrnoScanner(
	onHit: (match: string) => void
): (bytes: Uint8Array) => void {
	const decoder = new TextDecoder('utf-8', { fatal: false });
	let carry = '';
	return (bytes) => {
		const text = stripAnsi(carry + decoder.decode(bytes, { stream: true }));
		const m = NETWORK_ERRNO_RE.exec(text);
		if (m) {
			// Never re-report the same text from the carried tail.
			carry = '';
			onHit(m[0]);
			return;
		}
		carry = text.slice(-CARRY);
	};
}

/** Don't force more than one probe per distro in this window. */
export const ERRNO_PROBE_DEBOUNCE_MS = 30_000;

/**
 * Whether an errno seen now should force a fresh probe: not when a probe of
 * this distro is in flight, not when one was forced within the debounce
 * window, and not when the cached result already reports a problem (that
 * episode is known — its banner and notification are up).
 */
export function shouldForceProbeOnErrno(args: {
	cachedState: string | null | undefined;
	lastForcedAt: number | null | undefined;
	inFlight: boolean;
	now: number;
}): boolean {
	if (args.inFlight) return false;
	if (args.cachedState && args.cachedState !== 'ok') return false;
	if (args.lastForcedAt != null && args.now - args.lastForcedAt < ERRNO_PROBE_DEBOUNCE_MS) {
		return false;
	}
	return true;
}
