// Samples the round trip to the server for the connection indicator.
//
// Cost: one tiny frame each way every INTERVAL_MS, over the events socket the
// page already holds open (the server answers `ping` with `pong` without
// touching anything). A server too old for `ping`, or a page with no events
// socket, falls back to a timed `GET /api/health` (unauthenticated, ~200
// bytes). Nothing is measured while the tab is hidden, and nothing at all on
// the desktop, where the "server" is this process.
//
// The monitor is shared: every `useConnectionRtt()` consumer retains it, the
// last release stops it.

import { useCallback, useSyncExternalStore } from 'react';
import { getTransport, isBrowserSession, WebRemoteTransport } from '@/lib/transport';
import { PingError } from '@/lib/transport/events-socket';
import { EMPTY_RTT, RTT_WINDOW, summarizeRtt, type RttSummary } from './rtt';

/** Between probes while the tab is visible. */
export const INTERVAL_MS = 5_000;
/** A probe with no answer by now counts as a miss. */
export const PROBE_TIMEOUT_MS = 4_000;
/** Hidden for longer than this and the old samples describe a different
 *  moment: start the window over. */
export const STALE_AFTER_HIDDEN_MS = 60_000;

/** One measurement: milliseconds, or `null` for no answer. */
export type ProbeResult = { ms: number; via: 'ws' | 'http' } | null;

export interface Visibility {
	isHidden(): boolean;
	/** Calls back on every visibility change; returns the unsubscribe. */
	onChange(cb: () => void): () => void;
}

export interface RttMonitorDeps {
	probe: () => Promise<ProbeResult>;
	visibility: Visibility;
	now?: () => number;
	setTimer?: (fn: () => void, ms: number) => unknown;
	clearTimer?: (id: unknown) => void;
	intervalMs?: number;
}

export class RttMonitor {
	private readonly deps: Required<RttMonitorDeps>;
	private samples: number[] = [];
	private failures = 0;
	private via: RttSummary['via'] = null;
	private summary: RttSummary = EMPTY_RTT;
	private readonly listeners = new Set<() => void>();
	private holders = 0;
	private timer: unknown = null;
	private inFlight = false;
	private unsubVisibility: (() => void) | null = null;
	private hiddenAt: number | null = null;
	/** Bumped on stop and on a window reset so a late probe is ignored. */
	private epoch = 0;

	constructor(deps: RttMonitorDeps) {
		this.deps = {
			now: () => Date.now(),
			setTimer: (fn, ms) => setTimeout(fn, ms),
			clearTimer: (id) => clearTimeout(id as ReturnType<typeof setTimeout>),
			intervalMs: INTERVAL_MS,
			...deps,
		};
	}

	/** Start (first holder) sampling; returns the release. */
	retain(): () => void {
		this.holders++;
		if (this.holders === 1) this.start();
		let released = false;
		return () => {
			if (released) return;
			released = true;
			this.holders--;
			if (this.holders === 0) this.stop();
		};
	}

	get = (): RttSummary => this.summary;

	subscribe = (cb: () => void): (() => void) => {
		this.listeners.add(cb);
		return () => {
			this.listeners.delete(cb);
		};
	};

	/** Whether a timer or a probe is live (tests). */
	get active(): boolean {
		return this.timer !== null || this.inFlight;
	}

	private start(): void {
		this.unsubVisibility = this.deps.visibility.onChange(() => this.onVisibility());
		if (this.deps.visibility.isHidden()) {
			this.hiddenAt = this.deps.now();
		} else {
			void this.tick();
		}
	}

	private stop(): void {
		this.epoch++;
		this.clear();
		this.unsubVisibility?.();
		this.unsubVisibility = null;
		this.hiddenAt = null;
		this.inFlight = false;
		this.samples = [];
		this.failures = 0;
		this.via = null;
		this.set(EMPTY_RTT);
	}

	private clear(): void {
		if (this.timer !== null) {
			this.deps.clearTimer(this.timer);
			this.timer = null;
		}
	}

	private onVisibility(): void {
		if (this.holders === 0) return;
		if (this.deps.visibility.isHidden()) {
			this.hiddenAt = this.deps.now();
			this.clear();
			return;
		}
		const wasHiddenFor = this.hiddenAt === null ? 0 : this.deps.now() - this.hiddenAt;
		this.hiddenAt = null;
		if (wasHiddenFor > STALE_AFTER_HIDDEN_MS) {
			this.epoch++;
			this.samples = [];
			this.failures = 0;
			this.inFlight = false;
			this.set(EMPTY_RTT);
		}
		this.clear();
		void this.tick();
	}

	private async tick(): Promise<void> {
		if (this.inFlight || this.holders === 0 || this.deps.visibility.isHidden()) return;
		this.inFlight = true;
		const epoch = this.epoch;
		let result: ProbeResult = null;
		try {
			result = await this.deps.probe();
		} catch {
			result = null;
		}
		if (epoch !== this.epoch) return; // stopped or reset while waiting
		this.inFlight = false;
		if (result) {
			this.samples = [...this.samples, result.ms].slice(-RTT_WINDOW);
			this.failures = 0;
			this.via = result.via;
		} else {
			this.failures++;
		}
		this.set(summarizeRtt(this.samples, this.failures, this.via));
		// Only a visible, held monitor schedules the next probe.
		if (this.holders > 0 && !this.deps.visibility.isHidden()) {
			this.clear();
			this.timer = this.deps.setTimer(() => {
				this.timer = null;
				void this.tick();
			}, this.deps.intervalMs);
		}
	}

	private set(next: RttSummary): void {
		this.summary = next;
		for (const l of [...this.listeners]) l();
	}
}

// ─── the page's monitor ─────────────────────────────────────────────────────

const documentVisibility: Visibility = {
	isHidden: () => typeof document !== 'undefined' && document.visibilityState === 'hidden',
	onChange: (cb) => {
		if (typeof document === 'undefined') return () => {};
		document.addEventListener('visibilitychange', cb);
		return () => document.removeEventListener('visibilitychange', cb);
	},
};

/** Time `GET /api/health` to its body: the fallback probe. */
export async function httpProbe(timeoutMs = PROBE_TIMEOUT_MS): Promise<ProbeResult> {
	const ctl = new AbortController();
	const timer = setTimeout(() => ctl.abort(), timeoutMs);
	try {
		const t0 = performance.now();
		const res = await fetch('/api/health', {
			cache: 'no-store',
			credentials: 'same-origin',
			signal: ctl.signal,
		});
		await res.arrayBuffer();
		// A 5xx is the server answering, but not a clean measurement.
		return res.status >= 500 ? null : { ms: performance.now() - t0, via: 'http' };
	} catch {
		return null;
	} finally {
		clearTimeout(timer);
	}
}

/** The real probe: the events socket's ping, else the HTTP fallback. */
export async function pageProbe(): Promise<ProbeResult> {
	const transport = getTransport();
	if (transport instanceof WebRemoteTransport) {
		try {
			return { ms: await transport.pingEvents(PROBE_TIMEOUT_MS), via: 'ws' };
		} catch (e) {
			// A stalled link is a miss; the HTTP fallback would only stall too.
			if (e instanceof PingError && (e.reason === 'timeout' || e.reason === 'closed')) {
				return null;
			}
		}
	}
	return httpProbe();
}

let pageMonitor: RttMonitor | null = null;

export function getRttMonitor(): RttMonitor {
	pageMonitor ??= new RttMonitor({ probe: pageProbe, visibility: documentVisibility });
	return pageMonitor;
}

/** Test seam. */
export function __setRttMonitorForTests(m: RttMonitor | null): void {
	pageMonitor = m;
}

/** Whether this tab should measure at all: the SPA in a browser tab on a
 *  daemon. The desktop's "server" is its own process, and a test harness or
 *  Node import has no server (`isBrowserSession`, like the other
 *  browser-only behaviours). */
export function mayMeasureConnection(): boolean {
	return typeof window !== 'undefined' && isBrowserSession();
}

/** The current summary, measuring while mounted. Desktop: always empty. */
export function useConnectionRtt(enabled = true): RttSummary {
	const monitor = getRttMonitor();
	const on = enabled && mayMeasureConnection();
	// Stable identity is load-bearing: useSyncExternalStore resubscribes
	// whenever `subscribe` changes, and every resubscribe releases + retains the
	// monitor, which stops it (clearing the samples) and probes again at once.
	// An inline closure did that on every render: ~150 pings/s, no reading.
	const subscribe = useCallback(
		(cb: () => void) => {
			if (!on) return () => {};
			const release = monitor.retain();
			const unsub = monitor.subscribe(cb);
			return () => {
				unsub();
				release();
			};
		},
		[monitor, on]
	);
	return useSyncExternalStore(
		subscribe,
		() => (on ? monitor.get() : EMPTY_RTT),
		() => EMPTY_RTT
	);
}
