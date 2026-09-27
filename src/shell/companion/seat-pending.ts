// WP-67 — the seats inside their 8 s *Clear* window (G-SEATS §4.2), shared
// by the rail and the dispatch path. It has no imports, so `resolve-target`
// can settle a pending Clear before it routes a send without an import cycle
// through `seat-actions`.
//
// While a Clear is pending the rail shows the seat vacant with no history,
// but the host still holds its session. Anything that then occupies or
// removes the seat (a dispatch, *Fill*, *Remove*) first commits the Clear, so
// the send starts from what the rail showed and the timer can never fire
// later and clear a session that was started meanwhile.

interface PendingClear {
	timer: ReturnType<typeof setTimeout>;
	/** Makes the host call and settles the rail's overlay. Never throws. */
	commit: () => Promise<void>;
}

const pendingClears = new Map<string, PendingClear>();

export function hasPendingClear(seatId: string): boolean {
	return pendingClears.has(seatId);
}

/** Arm a Clear: `commit` runs when the window closes (or when flushed). */
export function armPendingClear(seatId: string, delayMs: number, commit: () => Promise<void>): void {
	const timer = setTimeout(() => {
		pendingClears.delete(seatId);
		void commit();
	}, delayMs);
	pendingClears.set(seatId, { timer, commit });
}

/** Undo: drop the pending Clear without calling the host. */
export function cancelPendingClear(seatId: string): boolean {
	const p = pendingClears.get(seatId);
	if (!p) return false;
	clearTimeout(p.timer);
	pendingClears.delete(seatId);
	return true;
}

/** Commit a pending Clear now (no-op when none is pending). Never throws. */
export async function flushPendingClear(seatId: string): Promise<void> {
	const p = pendingClears.get(seatId);
	if (!p) return;
	clearTimeout(p.timer);
	pendingClears.delete(seatId);
	await p.commit();
}

/** Test seam: drop every pending Clear without calling the host. */
export function __resetPendingClearsForTests(): void {
	for (const p of pendingClears.values()) clearTimeout(p.timer);
	pendingClears.clear();
}
