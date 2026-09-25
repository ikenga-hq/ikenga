// Which tab of the ONE update sheet (`src/shell/updater/update-sheet.tsx`) is
// open, and from which source. D-07 `update-flow` draws the shell updater and
// the package-updates batch as two tabs of the same sheet ("Shell" /
// "Packages (N)", `designs/system-flows.html` `UP.mode`), reached from either
// <UpdaterBanner> (shell binary) or <PkgAutoUpdater> (packages) — WP-41's
// "one surface, two sources". A tiny shared store (rather than prop-drilling
// between the two sibling banner components) is what lets either one open the
// single sheet instance mounted once in `workspace.tsx`.

import { create } from 'zustand';

export type UpdateSheetSource = 'shell' | 'pkgs';

interface UpdateSheetState {
	open: boolean;
	source: UpdateSheetSource;
	openSheet: (source: UpdateSheetSource) => void;
	close: () => void;
}

export const useUpdateSheetStore = create<UpdateSheetState>((set) => ({
	open: false,
	source: 'shell',
	openSheet: (source) => set({ open: true, source }),
	close: () => set({ open: false }),
}));
