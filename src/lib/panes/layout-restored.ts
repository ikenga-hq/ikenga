// One signal: the workspace has finished restoring the saved pane layout.
//
// `useWorkspaceEffects` restores the layout asynchronously after boot and
// then REPLACES the pane tree (`hydrate`). Anything that opens a tab at boot
// (the share landing, share-landing.ts) must wait for that, or its tab is
// wiped by the restore. Marked once per page load, whether or not a snapshot
// was found.

let restored = false;
const waiters = new Set<() => void>();

/** The workspace's layout restore has finished (called once, after hydrate). */
export function markLayoutRestored(): void {
	restored = true;
	for (const w of waiters) w();
	waiters.clear();
}

/** Whether the layout restore has finished. */
export function isLayoutRestored(): boolean {
	return restored;
}

/**
 * Resolves `true` once the layout is restored, or `false` after `timeoutMs`
 * (the restore never ran or hung): the caller goes ahead either way.
 */
export function whenLayoutRestored(timeoutMs: number): Promise<boolean> {
	if (restored) return Promise.resolve(true);
	return new Promise((resolve) => {
		const done = () => {
			clearTimeout(timer);
			resolve(true);
		};
		const timer = setTimeout(() => {
			waiters.delete(done);
			resolve(false);
		}, timeoutMs);
		waiters.add(done);
	});
}

/** Tests only: forget the mark. */
export function resetLayoutRestoredForTests(): void {
	restored = false;
	waiters.clear();
}
