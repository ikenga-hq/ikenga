// Cold-start timing harness for measuring perf changes against a
// stable baseline. Marks fire at four checkpoints:
//
//   1. boot:html             — index.html starts parsing (set there directly)
//   2. boot:js-start         — main.tsx top of file
//   3. boot:workspace-mount  — Workspace component first mount
//   4. boot:workspace-ready  — initialSizes resolved, workspace is interactive
//
// `dumpBootTimings()` computes deltas, writes them to console.table, and
// appends to a localStorage ring buffer (`__boot_timings__`) so a series
// of runs can be averaged without a profiler.

const STORAGE_KEY = '__boot_timings__';
const RING_SIZE = 20;

export type BootMark =
	| 'boot:html'
	| 'boot:js-start'
	| 'boot:workspace-mount'
	| 'boot:workspace-ready';

export function mark(name: BootMark): void {
	try {
		performance.mark(name);
	} catch {
		// performance.mark is universally available; the catch is paranoia.
	}
}

interface BootTimings {
	recordedAt: number;
	htmlToJsMs: number | null;
	jsToMountMs: number | null;
	mountToReadyMs: number | null;
	htmlToReadyMs: number | null;
	totalMs: number | null;
}

function entryTime(name: BootMark): number | null {
	const entries = performance.getEntriesByName(name, 'mark');
	return entries.length === 0 ? null : entries[entries.length - 1].startTime;
}

export function computeBootTimings(): BootTimings {
	const html = entryTime('boot:html');
	const js = entryTime('boot:js-start');
	const mount = entryTime('boot:workspace-mount');
	const ready = entryTime('boot:workspace-ready');

	const sub = (a: number | null, b: number | null) =>
		a === null || b === null ? null : Math.round(b - a);

	return {
		recordedAt: Date.now(),
		htmlToJsMs: sub(html, js),
		jsToMountMs: sub(js, mount),
		mountToReadyMs: sub(mount, ready),
		htmlToReadyMs: sub(html, ready),
		totalMs: ready,
	};
}

function loadHistory(): BootTimings[] {
	try {
		const raw = localStorage.getItem(STORAGE_KEY);
		return raw ? (JSON.parse(raw) as BootTimings[]) : [];
	} catch {
		return [];
	}
}

function saveHistory(history: BootTimings[]): void {
	try {
		localStorage.setItem(STORAGE_KEY, JSON.stringify(history.slice(-RING_SIZE)));
	} catch {
		// quota or disabled — drop silently.
	}
}

/** Compute, persist, and log the current boot's timings. Call once per
 *  session, ideally from the workspace's "ready" effect. */
export function dumpBootTimings(): BootTimings {
	const t = computeBootTimings();
	const history = [...loadHistory(), t];
	saveHistory(history);
	// eslint-disable-next-line no-console
	console.info('[boot-timing] this run', t);
	// eslint-disable-next-line no-console
	console.info('[boot-timing] history (last %d)', history.length, history);
	return t;
}

/** Average over the persisted ring buffer with best/worst trimmed. Lets
 *  you copy-paste a number after running 5–7 cold starts on each branch. */
export function averageBootTimings(): {
	count: number;
	htmlToReadyMs: number | null;
	totalMs: number | null;
} {
	const history = loadHistory().filter((h) => h.htmlToReadyMs !== null);
	if (history.length < 3) {
		return { count: history.length, htmlToReadyMs: null, totalMs: null };
	}
	const trim = (key: 'htmlToReadyMs' | 'totalMs') => {
		const xs = history
			.map((h) => h[key])
			.filter((v): v is number => v !== null)
			.sort((a, b) => a - b);
		if (xs.length < 3) return null;
		// Drop best+worst, average the middle.
		const middle = xs.slice(1, -1);
		return Math.round(middle.reduce((a, b) => a + b, 0) / middle.length);
	};
	return {
		count: history.length,
		htmlToReadyMs: trim('htmlToReadyMs'),
		totalMs: trim('totalMs'),
	};
}

/** Wipe the ring buffer — call before a fresh measurement series. */
export function resetBootTimings(): void {
	try {
		localStorage.removeItem(STORAGE_KEY);
	} catch {
		// ignore
	}
}

// Expose on window in dev for manual REPL inspection.
if (import.meta.env?.DEV && typeof window !== 'undefined') {
	type Win = Window & {
		__bootTimings?: {
			compute: typeof computeBootTimings;
			average: typeof averageBootTimings;
			reset: typeof resetBootTimings;
			dump: typeof dumpBootTimings;
		};
	};
	(window as Win).__bootTimings = {
		compute: computeBootTimings,
		average: averageBootTimings,
		reset: resetBootTimings,
		dump: dumpBootTimings,
	};
}
