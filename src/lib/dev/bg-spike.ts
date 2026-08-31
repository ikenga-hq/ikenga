// Phase 0.5 background-execution spike — FE side.
// Pairs with src-tauri/src/commands/bg_spike.rs.
//
// Two surfaces:
//   1. window.__bgSpikeReply(nonce) — called from Rust eval; invokes back
//      into Rust to close the round-trip. Auto-installed in dev builds.
//   2. window.bgSpikeRun(opts?) — convenience wrapper around the typed
//      command in tauri-cmd.ts, prints a one-line summary to console and
//      returns the full report.
//
// Workflow:
//   await window.bgSpikeRun()                         // 60s focused baseline
//   // minimize the window, then…
//   await window.bgSpikeRun({tag:'minimized'})        // 60s minimized
//   // background the app behind another, then…
//   await window.bgSpikeRun({tag:'backgrounded'})     // 60s backgrounded
//
// Delete this file + the import in dev/index.ts once Phase 0.5 sign-off lands.

import { invoke } from '@/lib/transport';
import { bgSpikeRun, type BgSpikeReport } from '@/lib/tauri-cmd';

declare global {
	interface Window {
		__bgSpikeReply?: (nonce: number) => void;
		bgSpikeRun?: (opts?: BgSpikeRunOpts) => Promise<BgSpikeReport>;
	}
}

interface BgSpikeRunOpts {
	durationMs?: number;
	intervalMs?: number;
	perPingTimeoutMs?: number;
	tag?: string;
}

function installReplyHook(): void {
	if (window.__bgSpikeReply) return;
	window.__bgSpikeReply = (nonce: number) => {
		// Fire-and-forget; the Rust side records its own t1 the moment the
		// invoke arrives, so we don't need to await or measure here.
		void invoke('bg_spike_reply', { nonce });
	};
}

async function run(opts: BgSpikeRunOpts = {}): Promise<BgSpikeReport> {
	installReplyHook();
	const durationMs = opts.durationMs ?? 60_000;
	const intervalMs = opts.intervalMs ?? 500;
	const perPingTimeoutMs = opts.perPingTimeoutMs ?? 5_000;
	const tag = opts.tag ?? 'baseline';
	console.log(
		`[bg-spike] starting tag=${tag} duration=${durationMs}ms interval=${intervalMs}ms timeout=${perPingTimeoutMs}ms`
	);
	const t0 = performance.now();
	const r = await bgSpikeRun(durationMs, intervalMs, perPingTimeoutMs);
	const wall = performance.now() - t0;
	const oneLine = {
		tag,
		intended: r.intendedCount,
		done: r.completedCount,
		timeouts: r.timeoutCount,
		p50_ms: +(r.p50Us / 1000).toFixed(2),
		p95_ms: +(r.p95Us / 1000).toFixed(2),
		p99_ms: +(r.p99Us / 1000).toFixed(2),
		max_ms: +(r.maxUs / 1000).toFixed(2),
		webview: r.webviewLabel,
		wall_ms: Math.round(wall),
	};
	console.log('[bg-spike] result', oneLine);
	console.table(oneLine);
	return r;
}

if (typeof window !== 'undefined') {
	installReplyHook();
	window.bgSpikeRun = run;
}

export { run as bgSpikeDevRun, installReplyHook as installBgSpikeHook };
