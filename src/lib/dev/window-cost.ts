// Multi-window WP-01 window cost spike — FE side.
// Pairs with src-tauri/src/commands/window_cost.rs.
//
// Installs two globals on `window` in dev builds:
//
//   window.__windowCostPing()   — called from Rust eval() inside
//                                  window_cost_run to signal first-paint;
//                                  fires the `window_cost_ping` Rust command.
//
//   window.windowCostRun()      — convenience wrapper: invokes `window_cost_run`
//                                  and pretty-prints the cost table to console.
//
// Usage (DevTools console, dev build, from the MAIN window):
//
//   // Kick off the three-config measurement sequence (~5–45 s depending on
//   // first-paint latency of thin + full windows). The command spawns three
//   // throwaway windows in sequence, samples their renderer RSS, then closes
//   // them. Progress is visible as new windows flash briefly on screen.
//   await window.windowCostRun();
//
// Or via the typed tauri-cmd wrapper (e.g. from a dev-only component):
//
//   import { windowCostRun } from '@/lib/tauri-cmd';
//   const r = await windowCostRun();
//   console.table(r.rows);
//
// The `__windowCostPing` hook is also auto-installed in every window that
// loads this dev bundle (thin + full probe windows in config b + c). Rust
// polls eval(PING_JS) into those windows until the hook fires, giving the
// first-paint latency from the host side.
//
// Hard-removed after Phase-3 sign-off alongside window_cost.rs.

import { invoke } from '@tauri-apps/api/core';

declare global {
	interface Window {
		/** Called by Rust eval() inside window_cost_run to signal first-paint. */
		__windowCostPing?: () => void;
		/** Convenience console helper: runs the spike and prints a cost table. */
		windowCostRun?: () => Promise<WindowCostReport>;
	}
}

export interface WindowCostRow {
	config: string;
	spawnMs: number;
	firstPaintMs: number | null;
	rssKb: number | null;
	rssNote: string;
	error: string | null;
}

export interface WindowCostReport {
	rows: WindowCostRow[];
	os: string;
	osRssNote: string;
}

/** Install `window.__windowCostPing`. Idempotent. */
function installPingHook(): void {
	if (window.__windowCostPing) return;
	window.__windowCostPing = (): void => {
		// Fire-and-forget; Rust records its own Instant::now() on arrival so
		// we don't need to measure anything here.
		void invoke('window_cost_ping', {});
	};
}

/** Run the cost spike and print results. */
async function run(): Promise<WindowCostReport> {
	console.log('[window-cost] starting — three windows will appear briefly…');
	const t0 = performance.now();
	const r = await invoke<WindowCostReport>('window_cost_run', {});
	const wall = Math.round(performance.now() - t0);

	console.log(`[window-cost] done in ${wall} ms (OS: ${r.os})`);
	console.log('[window-cost] RSS note:', r.osRssNote);
	console.table(
		r.rows.map((row) => ({
			config: row.config,
			spawn_ms: row.spawnMs,
			first_paint_ms: row.firstPaintMs ?? '—',
			rss_kb: row.rssKb ?? '(see note)',
			error: row.error ?? '',
		}))
	);
	return r;
}

if (typeof window !== 'undefined') {
	installPingHook();
	window.windowCostRun = run;
}

export { installPingHook };
