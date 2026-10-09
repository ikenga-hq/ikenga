// Connection quality for a browser viewer: round-trip time to the server,
// summarised as a median and a jitter, and the thresholds that turn it amber
// or red. Pure functions only; the sampling lives in `rtt-monitor.ts`.
//
// Why these numbers. A terminal keystroke is echoed by the server, so every
// typed character waits one round trip before it shows. Under ~150 ms that is
// not noticeable; past ~300 ms it is the "typing lags" feeling. Jitter is the
// uneven part: a steady 300 ms is livable, 300 ± 150 ms is stutter.

/** At or below this the connection is fine. */
export const RTT_GOOD_MS = 150;
/** Above this typing visibly lags (between the two is amber). */
export const RTT_BAD_MS = 300;
/** Samples kept: about a minute at the monitor's 5 s cadence. */
export const RTT_WINDOW = 12;

export type RttLevel = 'good' | 'warn' | 'bad' | 'unknown';

/** Median of the samples; `null` when there are none. */
export function median(samples: readonly number[]): number | null {
	if (samples.length === 0) return null;
	const s = [...samples].sort((a, b) => a - b);
	const mid = s.length >> 1;
	return s.length % 2 === 1 ? s[mid]! : (s[mid - 1]! + s[mid]!) / 2;
}

/**
 * Jitter: the mean absolute difference between consecutive samples (the
 * RFC 3550 notion), in the order they were taken. Needs three samples to
 * say anything; fewer is `null` rather than a confident zero.
 */
export function jitter(samples: readonly number[]): number | null {
	if (samples.length < 3) return null;
	let sum = 0;
	for (let i = 1; i < samples.length; i++) sum += Math.abs(samples[i]! - samples[i - 1]!);
	return sum / (samples.length - 1);
}

/** The colour a median earns. */
export function classifyRtt(medianMs: number | null): RttLevel {
	if (medianMs === null || !Number.isFinite(medianMs)) return 'unknown';
	if (medianMs > RTT_BAD_MS) return 'bad';
	if (medianMs > RTT_GOOD_MS) return 'warn';
	return 'good';
}

export interface RttSummary {
	/** Samples in the window. */
	count: number;
	medianMs: number | null;
	jitterMs: number | null;
	level: RttLevel;
	/** Probes in a row that got no answer. Two or more means the link is
	 *  stalled, whatever the older samples say. */
	failures: number;
	/** How the last sample was taken. */
	via: 'ws' | 'http' | null;
}

export const EMPTY_RTT: RttSummary = {
	count: 0,
	medianMs: null,
	jitterMs: null,
	level: 'unknown',
	failures: 0,
	via: null,
};

/** Two probes without an answer in a row: stalled. */
export const STALLED_AFTER = 2;

export function summarizeRtt(
	samples: readonly number[],
	failures: number,
	via: RttSummary['via']
): RttSummary {
	const medianMs = median(samples);
	const stalled = failures >= STALLED_AFTER;
	return {
		count: samples.length,
		medianMs,
		jitterMs: jitter(samples),
		level: stalled ? 'bad' : classifyRtt(medianMs),
		failures,
		via,
	};
}

/** "340 ms ± 85". Jitter is left off until it can be computed. */
export function formatRtt(s: RttSummary): string {
	if (s.failures >= STALLED_AFTER) return 'No response';
	if (s.medianMs === null) return 'Measuring…';
	const m = `${Math.round(s.medianMs)} ms`;
	return s.jitterMs === null ? m : `${m} ± ${Math.round(s.jitterMs)}`;
}

/** The sentence under the number: what it means for typing. */
export function explainRtt(s: RttSummary): string {
	if (s.failures >= STALLED_AFTER) {
		return 'The server has not answered the last few checks. Keystrokes may not be arriving.';
	}
	switch (s.level) {
		case 'good':
			return 'Typing should feel immediate.';
		case 'warn':
			return 'Each keystroke waits this long for the server to echo it, so typing may feel slightly delayed.';
		case 'bad':
			return 'Each keystroke waits this long for the server to echo it. Typing will lag; a steadier connection helps (Wi-Fi over mobile data, or a closer network).';
		default:
			return 'Measuring the round trip to the server.';
	}
}
