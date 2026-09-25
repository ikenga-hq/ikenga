// WP-45 — D-08 `pkg-blocked` secondary action ("Keep blocking") for webview
// panes. The state itself offers one action (Allow host…); the design's
// Keep blocking lives in the pane `⋯` menu (`pkg-pane-menu.tsx`). A webview
// host that parked its native surface on a blocked navigation registers a
// restore callback here, keyed by (pkgId, paneId); the menu shows the item
// only while one is registered.

import { create } from 'zustand';

type Key = string;

const keyOf = (pkgId: string, paneId: string): Key => `${pkgId}\u0000${paneId}`;

interface PkgBlockedStore {
	keepBlocking: Record<Key, () => void>;
	/** Register the restore callback; returns the unregister function. */
	register: (pkgId: string, paneId: string, onKeepBlocking: () => void) => () => void;
}

export const usePkgBlockedStore = create<PkgBlockedStore>((set, get) => ({
	keepBlocking: {},
	register: (pkgId, paneId, onKeepBlocking) => {
		const key = keyOf(pkgId, paneId);
		set((s) => ({ keepBlocking: { ...s.keepBlocking, [key]: onKeepBlocking } }));
		return () => {
			if (get().keepBlocking[key] !== onKeepBlocking) return;
			set((s) => {
				const { [key]: _drop, ...rest } = s.keepBlocking;
				return { keepBlocking: rest };
			});
		};
	},
}));

export function useKeepBlocking(pkgId: string, paneId: string): (() => void) | undefined {
	return usePkgBlockedStore((s) => s.keepBlocking[keyOf(pkgId, paneId)]);
}
