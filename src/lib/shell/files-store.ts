// Files explorer state — survives mode switches (Zustand) and full reload
// (SQLite via layout-state). Children listings are owned by TanStack Query
// keyed by path; this store only persists what the FS itself can't tell us:
// which folders the user has expanded, which file is selected, and where
// the tree was scrolled — re-keyed per active project.

import { create } from 'zustand';
import { debounce, loadLayoutState, saveLayoutState } from '@/lib/layout-state';
import { useShellStore } from './shell-store';

export const STORAGE_KEY = 'files.explorer.v1';

export interface ProjectFilesPersisted {
	expanded: string[];
	selectedPath: string | null;
	scrollTop: number;
	showHidden: boolean;
	showIgnored: boolean;
	expandedRoot: string | null;
}

export interface FilesPersisted {
	byProject: Record<string, ProjectFilesPersisted>;
	// Legacy fields for backward compatibility / migration
	expanded?: string[];
	selectedPath?: string | null;
	scrollTop?: number;
	showHidden?: boolean;
	showIgnored?: boolean;
	expandedRoot?: string | null;
}

export interface FilesState {
	activeProjectId: string;
	byProject: Record<string, ProjectFilesPersisted>;
	expanded: Set<string>;
	selectedPath: string | null;
	scrollTop: number;
	/** Show dotfile-prefixed entries (e.g. `.git`, `.next`, `.env`). Default off. */
	showHidden: boolean;
	/** Show heavy-ignored directories (`node_modules`, `target`, `dist`, etc.).
	 * Lazy expansion still applies — toggling this on does NOT auto-expand the
	 * dirs, so it stays cheap until the user clicks one. Default off. */
	showIgnored: boolean;
	/** Per-root search query. Transient — not persisted. Empty/missing means
	 *  no filter is active for that root. */
	queries: Record<string, string>;
	/** The single root section currently expanded (accordion). `null` = all
	 *  collapsed. Persisted. Inner-folder state lives in `expanded` (keyed by
	 *  absolute path), so switching roots preserves each root's prior tree. */
	expandedRoot: string | null;
	hydrated: boolean;
	hydrate: () => Promise<void>;
	snapshot: () => FilesPersisted;
	setActiveProjectId: (projectId: string) => void;
	toggle: (path: string) => void;
	expand: (path: string) => void;
	collapse: (path: string) => void;
	setSelected: (path: string | null) => void;
	setScrollTop: (n: number) => void;
	setShowHidden: (b: boolean) => void;
	setShowIgnored: (b: boolean) => void;
	toggleShowHidden: () => void;
	toggleShowIgnored: () => void;
	setQuery: (rootPath: string, query: string) => void;
	/** Set which root section is expanded (accordion). `null` collapses all. */
	setExpandedRoot: (rootPath: string | null) => void;
	/** Toggle a root: open it, or collapse it if it's already the open one. */
	toggleRoot: (rootPath: string) => void;
	/** Reveal a file: open its root, expand the ancestor dirs leading to it, and
	 *  select it — all in one update so the tree doesn't churn. */
	reveal: (rootPath: string, ancestors: string[], selectedPath: string) => void;
	/** Drop paths from the expanded set (used after rename/trash/missing). */
	prune: (paths: string[]) => void;
}

const persist = debounce((data: FilesPersisted) => {
	void saveLayoutState(STORAGE_KEY, data);
}, 250);

const EMPTY_PROJECT_PERSISTED: ProjectFilesPersisted = {
	expanded: [],
	selectedPath: null,
	scrollTop: 0,
	showHidden: false,
	showIgnored: false,
	expandedRoot: null,
};

function currentProjectSlice(s: FilesState): ProjectFilesPersisted {
	return {
		expanded: [...s.expanded],
		selectedPath: s.selectedPath,
		scrollTop: s.scrollTop,
		showHidden: s.showHidden,
		showIgnored: s.showIgnored,
		expandedRoot: s.expandedRoot,
	};
}

function snapshotOf(s: FilesState): FilesPersisted {
	const curSlice = currentProjectSlice(s);
	const byProj = { ...s.byProject };
	if (s.activeProjectId) {
		byProj[s.activeProjectId] = curSlice;
	}
	return {
		byProject: byProj,
	};
}

function persistCurrent(get: () => FilesState): void {
	if (!get().hydrated) return;
	persist(snapshotOf(get()));
}

const initialProjectId = typeof useShellStore !== 'undefined'
	? (useShellStore.getState().activeProjectId || 'default')
	: 'default';

export const useFilesStore = create<FilesState>((set, get) => ({
	activeProjectId: initialProjectId,
	byProject: {},
	expanded: new Set<string>(),
	selectedPath: null,
	scrollTop: 0,
	showHidden: false,
	showIgnored: false,
	queries: {},
	expandedRoot: null,
	hydrated: false,

	hydrate: async () => {
		if (get().hydrated) return;
		const data = await loadLayoutState<FilesPersisted>(STORAGE_KEY, { byProject: {} });
		const byProject: Record<string, ProjectFilesPersisted> = { ...(data?.byProject ?? {}) };

		// Backwards compatibility / migration from v1 unkeyed layout:
		if (Array.isArray((data as unknown as ProjectFilesPersisted)?.expanded) && Object.keys(byProject).length === 0) {
			const legacy = data as unknown as ProjectFilesPersisted;
			byProject['default'] = {
				expanded: legacy.expanded ?? [],
				selectedPath: legacy.selectedPath ?? null,
				scrollTop: legacy.scrollTop ?? 0,
				showHidden: legacy.showHidden ?? false,
				showIgnored: legacy.showIgnored ?? false,
				expandedRoot: legacy.expandedRoot ?? null,
			};
		}

		const projId = useShellStore.getState().activeProjectId || 'default';
		const target = byProject[projId] ?? EMPTY_PROJECT_PERSISTED;

		set({
			activeProjectId: projId,
			byProject,
			expanded: new Set(target.expanded ?? []),
			selectedPath: target.selectedPath ?? null,
			scrollTop: target.scrollTop ?? 0,
			showHidden: target.showHidden ?? false,
			showIgnored: target.showIgnored ?? false,
			queries: {},
			expandedRoot: target.expandedRoot ?? null,
			hydrated: true,
		});
	},

	snapshot: () => snapshotOf(get()),

	setActiveProjectId: (projectId: string) => {
		const curId = get().activeProjectId;
		if (curId === projectId) return;

		const curSlice = currentProjectSlice(get());
		const nextByProject = {
			...get().byProject,
			[curId]: curSlice,
		};

		const target = nextByProject[projectId] ?? EMPTY_PROJECT_PERSISTED;

		set({
			activeProjectId: projectId,
			byProject: nextByProject,
			expanded: new Set(target.expanded ?? []),
			selectedPath: target.selectedPath ?? null,
			scrollTop: target.scrollTop ?? 0,
			showHidden: target.showHidden ?? false,
			showIgnored: target.showIgnored ?? false,
			queries: {},
			expandedRoot: target.expandedRoot ?? null,
		});

		persistCurrent(get);
	},

	toggle: (path) => {
		const next = new Set(get().expanded);
		if (next.has(path)) next.delete(path);
		else next.add(path);
		set({ expanded: next });
		persistCurrent(get);
	},

	expand: (path) => {
		if (get().expanded.has(path)) return;
		const next = new Set(get().expanded);
		next.add(path);
		set({ expanded: next });
		persistCurrent(get);
	},

	collapse: (path) => {
		if (!get().expanded.has(path)) return;
		const next = new Set(get().expanded);
		next.delete(path);
		set({ expanded: next });
		persistCurrent(get);
	},

	setSelected: (selectedPath) => {
		set({ selectedPath });
		persistCurrent(get);
	},

	setScrollTop: (scrollTop) => {
		set({ scrollTop });
		persistCurrent(get);
	},

	setShowHidden: (showHidden) => {
		if (get().showHidden === showHidden) return;
		set({ showHidden });
		persistCurrent(get);
	},

	setShowIgnored: (showIgnored) => {
		if (get().showIgnored === showIgnored) return;
		set({ showIgnored });
		persistCurrent(get);
	},

	toggleShowHidden: () => {
		set({ showHidden: !get().showHidden });
		persistCurrent(get);
	},

	toggleShowIgnored: () => {
		set({ showIgnored: !get().showIgnored });
		persistCurrent(get);
	},

	setQuery: (rootPath, query) => {
		const cur = get().queries;
		if ((cur[rootPath] ?? '') === query) return;
		const next = { ...cur };
		if (query === '') delete next[rootPath];
		else next[rootPath] = query;
		set({ queries: next });
	},

	setExpandedRoot: (rootPath) => {
		if (get().expandedRoot === rootPath) return;
		set({ expandedRoot: rootPath });
		persistCurrent(get);
	},

	toggleRoot: (rootPath) => {
		set({ expandedRoot: get().expandedRoot === rootPath ? null : rootPath });
		persistCurrent(get);
	},

	reveal: (rootPath, ancestors, selectedPath) => {
		const nextExpanded = new Set(get().expanded);
		for (const a of ancestors) nextExpanded.add(a);
		set({ expandedRoot: rootPath, expanded: nextExpanded, selectedPath });
		persistCurrent(get);
	},

	prune: (paths) => {
		const cur = get().expanded;
		let changed = false;
		const next = new Set(cur);
		for (const p of paths) {
			if (next.delete(p)) changed = true;
			// also prune descendants — if `p` was a directory, anything under it
			// is now stale.
			const prefix = p.endsWith('/') ? p : `${p}/`;
			for (const e of cur) {
				if (e.startsWith(prefix) && next.delete(e)) changed = true;
			}
		}
		if (!changed) return;
		set({ expanded: next });
		persistCurrent(get);
	},
}));

// Synchronously sync with shell store's activeProjectId in the same tick:
useShellStore.subscribe((state) => {
	const curStoreId = useFilesStore.getState().activeProjectId;
	const nextId = state.activeProjectId;
	if (nextId && nextId !== curStoreId) {
		useFilesStore.getState().setActiveProjectId(nextId);
	}
});

