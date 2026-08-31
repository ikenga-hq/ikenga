import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import {
	ChevronDown,
	ChevronRight,
	Folder,
	FileText,
	AlertCircle,
	Grid3x3,
	RefreshCw,
	Pencil,
	Trash2,
	MoreHorizontal,
	Search,
	X,
} from 'lucide-react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { writeClipboardText } from '@/lib/transport/shims';
import { useShellStore } from '@/lib/shell/shell-store';
import { useFilesStore } from '@/lib/shell/files-store';
import { usePaneStore } from '@/lib/panes/pane-store';
import { fsList, fsRename, fsSearch, fsTrash, type FileEntry } from '@/lib/tauri-cmd';
import { createTerminalSession } from '@/terminal/single-terminal';
import { openArtifactGrid } from '@/lib/shell/artifact-grid-recents';
import { PinArtifactDialog } from '@/shell/panes/pin-artifact-dialog';
import { queryKeys } from '@/lib/query-keys';
import { ListRow, RowAction } from '@/components/ui/list-row';
import { cn } from '@/components/ui/utils';
import {
	ContextMenu,
	ContextMenuContent,
	ContextMenuItem,
	ContextMenuSeparator,
	ContextMenuTrigger,
} from '@/components/ui/context-menu';
import {
	DropdownMenu,
	DropdownMenuCheckboxItem,
	DropdownMenuContent,
	DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { useGitStatus } from '@/lib/shell/use-git-status';

// Folders we never auto-list by default. The dot-file filter already catches
// `.git`, `.next`, `.cache`, `.turbo`, etc.; this catches the un-prefixed ones
// that can each hold tens of thousands of entries. Toggle off via the
// "Show ignored folders" view option (still lazy on expand).
const IGNORED_DIRS = new Set(['node_modules', 'target', 'dist', 'build', 'out']);

interface SortOptions {
	showHidden: boolean;
	showIgnored: boolean;
}

function sortEntries(list: FileEntry[], opts: SortOptions): FileEntry[] {
	const filtered = list.filter((e) => {
		if (!opts.showHidden && e.name.startsWith('.')) return false;
		if (!opts.showIgnored && e.isDir && IGNORED_DIRS.has(e.name)) return false;
		return true;
	});
	filtered.sort((a, b) => {
		if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
		return a.name.localeCompare(b.name);
	});
	return filtered;
}

function parentOf(p: string): string {
	const idx = p.lastIndexOf('/');
	return idx > 0 ? p.slice(0, idx) : p;
}

// 'right' splits horizontally (panes side by side), 'bottom' splits vertically
// (panes stacked). Matches the wording in the context menu.
type SplitMode = 'right' | 'bottom';

function openArtifactInSplit(path: string, mode: SplitMode) {
	// placeView with a split mode calls splitLeafAt under the hood — the new
	// leaf gets just the requested view, no clone of the active tab and no
	// cross-pane dedup. Using splitPane + addTab would clone the source pane's
	// active tab into the split AND run dedup that can steal focus back.
	const { focusedId, placeView } = usePaneStore.getState();
	placeView(focusedId, { kind: 'artifact', path }, mode);
}

function openTerminalAt(cwd: string, splitMode?: SplitMode) {
	const sessionId = createTerminalSession({ cwd });
	const view = { kind: 'terminal' as const, sessionId };
	const { focusedId, addTab, placeView } = usePaneStore.getState();
	if (splitMode) {
		placeView(focusedId, view, splitMode);
	} else {
		addTab(focusedId, view);
	}
}

// Build the visible set for a per-root search: every matched path plus every
// directory between it and the root (exclusive). Anything in this set is
// rendered; dirs in this set auto-expand without touching the persisted
// `expanded` state.
function buildMatchSet(matches: readonly string[], rootPath: string): Set<string> {
	const set = new Set<string>();
	const rootWithSlash = rootPath.endsWith('/') ? rootPath : `${rootPath}/`;
	for (const m of matches) {
		if (!m.startsWith(rootWithSlash)) continue;
		set.add(m);
		let cur = m;
		while (true) {
			const idx = cur.lastIndexOf('/');
			if (idx <= 0) break;
			cur = cur.slice(0, idx);
			if (cur === rootPath || !cur.startsWith(rootWithSlash)) break;
			set.add(cur);
		}
	}
	return set;
}

interface TreeNodeProps {
	entry: FileEntry;
	depth: number;
	/** When provided, the tree is in search-filter mode: only entries whose
	 *  path is in `filter` render, and directories in `filter` auto-expand
	 *  (without writing to the persisted `expanded` set). */
	filter?: Set<string>;
}

function TreeNode({ entry, depth, filter }: TreeNodeProps) {
	const persistedExpanded = useFilesStore((s) => s.expanded.has(entry.path));
	const expanded = filter ? filter.has(entry.path) && entry.isDir : persistedExpanded;
	const isSelected = useFilesStore((s) => s.selectedPath === entry.path);
	const toggle = useFilesStore((s) => s.toggle);
	const collapse = useFilesStore((s) => s.collapse);
	const setSelected = useFilesStore((s) => s.setSelected);
	const prune = useFilesStore((s) => s.prune);
	const showHidden = useFilesStore((s) => s.showHidden);
	const showIgnored = useFilesStore((s) => s.showIgnored);
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const qc = useQueryClient();

	const [renaming, setRenaming] = useState(false);
	const [renameValue, setRenameValue] = useState(entry.name);
	const [actionError, setActionError] = useState<string | null>(null);
	const [pinOpen, setPinOpen] = useState(false);
	const renameInputRef = useRef<HTMLInputElement | null>(null);
	const rowRef = useRef<HTMLDivElement | null>(null);

	// When this row becomes the selected one (e.g. a file was revealed after
	// being opened in a pane), scroll it into view within the open section's
	// scroller. Runs on the mount where `isSelected` is first true — which, for
	// a lazily-rendered deep node, is exactly when its ancestors finish loading.
	useEffect(() => {
		if (isSelected) rowRef.current?.scrollIntoView({ block: 'nearest' });
	}, [isSelected]);

	// Re-create the select fn when flags change so the visible list updates
	// immediately. TanStack Query memoizes by reference, so we need a new fn
	// identity here.
	const selectSorted = useMemo(
		() => (list: FileEntry[]) => sortEntries(list, { showHidden, showIgnored }),
		[showHidden, showIgnored]
	);

	const childrenQuery = useQuery({
		queryKey: queryKeys.fs.list(entry.path),
		queryFn: () => fsList(entry.path),
		enabled: entry.isDir && expanded,
		staleTime: 30_000,
		select: selectSorted,
	});

	// If the directory disappeared, prune it from expanded so we don't
	// keep retrying on next mount.
	useEffect(() => {
		if (childrenQuery.isError && expanded) {
			prune([entry.path]);
		}
	}, [childrenQuery.isError, expanded, entry.path, prune]);

	const openFile = useCallback(() => {
		setSelected(entry.path);
		const focusedId = usePaneStore.getState().focusedId;
		usePaneStore.getState().addTab(focusedId, { kind: 'artifact', path: entry.path });
	}, [entry.path, setSelected]);

	const handleClick = useCallback(() => {
		if (renaming) return;
		if (entry.isDir) toggle(entry.path);
		else openFile();
	}, [entry.isDir, entry.path, renaming, toggle, openFile]);

	const startRename = useCallback(() => {
		setRenameValue(entry.name);
		setActionError(null);
		setRenaming(true);
	}, [entry.name]);

	useEffect(() => {
		if (renaming) {
			renameInputRef.current?.focus();
			renameInputRef.current?.select();
		}
	}, [renaming]);

	const commitRename = useCallback(async () => {
		const next = renameValue.trim();
		if (!next || next === entry.name) {
			setRenaming(false);
			return;
		}
		try {
			await fsRename(entry.path, next);
			setRenaming(false);
			// Path changed — drop the old path from expanded, refresh parent listing.
			prune([entry.path]);
			void qc.invalidateQueries({ queryKey: queryKeys.fs.list(parentOf(entry.path)) });
		} catch (err) {
			setActionError(err instanceof Error ? err.message : String(err));
		}
	}, [renameValue, entry.path, entry.name, prune, qc]);

	const handleDelete = useCallback(async () => {
		const ok = window.confirm(`Move "${entry.name}" to trash?`);
		if (!ok) return;
		try {
			await fsTrash(entry.path);
			prune([entry.path]);
			void qc.invalidateQueries({ queryKey: queryKeys.fs.list(parentOf(entry.path)) });
		} catch (err) {
			setActionError(err instanceof Error ? err.message : String(err));
		}
	}, [entry.path, entry.name, prune, qc]);

	const handleRefresh = useCallback(() => {
		if (!entry.isDir) return;
		if (!expanded) {
			toggle(entry.path);
			return;
		}
		void qc.invalidateQueries({ queryKey: queryKeys.fs.list(entry.path) });
	}, [entry.isDir, entry.path, expanded, toggle, qc]);

	const children = childrenQuery.data;
	const error = childrenQuery.error
		? childrenQuery.error instanceof Error
			? childrenQuery.error.message
			: String(childrenQuery.error)
		: null;

	const terminalCwd = entry.isDir ? entry.path : parentOf(entry.path);
	const copyPath = useCallback(() => {
		void writeClipboardText(entry.path).catch(() => {});
	}, [entry.path]);

	const { data: gitStatus } = useGitStatus();
	const fileGitStatus = entry.isDir ? undefined : gitStatus?.files.get(entry.path);
	const isDirtyFolder = entry.isDir ? gitStatus?.dirtyFolders.has(entry.path) : false;

	return (
		<div>
			<ContextMenu>
				<ContextMenuTrigger asChild>
					<ListRow
						ref={rowRef}
						size="sm"
						selected={isSelected}
						onActivate={handleClick}
						indent={Math.min(depth, 10) * 12 + 8}
						title={entry.path}
						className="w-max min-w-full gap-1"
						actions={
							!renaming && (
								<div className="absolute right-1 top-1/2 hidden -translate-y-1/2 items-center gap-0.5 rounded bg-accent pl-1 group-hover/row:flex group-focus-within/row:flex">
									{entry.isDir && (
										<RowAction
											icon={<RefreshCw className="h-3 w-3" />}
											label="Refresh"
											onClick={handleRefresh}
										/>
									)}
									<RowAction
										icon={<Pencil className="h-3 w-3" />}
										label="Rename"
										onClick={startRename}
									/>
									<RowAction
										icon={<Trash2 className="h-3 w-3" />}
										label="Move to trash"
										onClick={() => void handleDelete()}
										danger
									/>
								</div>
							)
						}
					>
						{entry.isDir ? (
							expanded ? (
								<ChevronDown className="h-3 w-3 shrink-0" />
							) : (
								<ChevronRight className="h-3 w-3 shrink-0" />
							)
						) : (
							<span className="h-3 w-3 shrink-0" aria-hidden />
						)}
						{entry.isDir ? (
							<Folder className={cn('h-3.5 w-3.5 shrink-0', isDirtyFolder && 'text-amber-400/90')} />
						) : (
							<FileText className={cn(
								'h-3.5 w-3.5 shrink-0',
								fileGitStatus === 'modified' && 'text-amber-400',
								fileGitStatus === 'added' && 'text-emerald-400',
								fileGitStatus === 'untracked' && 'text-emerald-500/80',
								fileGitStatus === 'conflicted' && 'text-red-400'
							)} />
						)}
						{renaming ? (
							<input
								ref={renameInputRef}
								value={renameValue}
								onChange={(e) => setRenameValue(e.target.value)}
								onClick={(e) => e.stopPropagation()}
								onKeyDown={(e) => {
									e.stopPropagation();
									if (e.key === 'Enter') {
										e.preventDefault();
										void commitRename();
									} else if (e.key === 'Escape') {
										e.preventDefault();
										setRenaming(false);
									}
								}}
								onBlur={() => void commitRename()}
								className="w-full min-w-0 rounded border border-border bg-background px-1 py-0 text-xs text-foreground outline-none focus:border-ring"
							/>
						) : (
							<span className={cn(
								'flex-1 whitespace-nowrap',
								fileGitStatus === 'modified' && 'text-amber-400 font-medium',
								fileGitStatus === 'added' && 'text-emerald-400 font-medium',
								fileGitStatus === 'untracked' && 'text-emerald-500/80',
								fileGitStatus === 'conflicted' && 'text-red-400 font-medium'
							)}>
								{entry.name}
							</span>
						)}
						{entry.isDir && isDirtyFolder && (
							<span className="ml-1.5 h-1.5 w-1.5 rounded-full bg-amber-400/80 shrink-0" title="Contains modified files" />
						)}
						{!entry.isDir && fileGitStatus && (
							<span className={cn(
								'ml-auto font-mono text-[10px] font-semibold px-1 shrink-0',
								fileGitStatus === 'modified' && 'text-amber-400',
								fileGitStatus === 'added' && 'text-emerald-400',
								fileGitStatus === 'untracked' && 'text-emerald-500/80',
								fileGitStatus === 'conflicted' && 'text-red-400'
							)}>
								{fileGitStatus === 'modified' && 'M'}
								{fileGitStatus === 'added' && 'A'}
								{fileGitStatus === 'untracked' && 'U'}
								{fileGitStatus === 'conflicted' && 'C'}
							</span>
						)}
					</ListRow>
				</ContextMenuTrigger>
				<ContextMenuContent>
					{!entry.isDir && (
						<>
							<ContextMenuItem onSelect={() => openFile()}>Open</ContextMenuItem>
							<ContextMenuItem onSelect={() => openArtifactInSplit(entry.path, 'right')}>
								Open to the Side
							</ContextMenuItem>
							<ContextMenuItem onSelect={() => openArtifactInSplit(entry.path, 'bottom')}>
								Open Below
							</ContextMenuItem>
							<ContextMenuSeparator />
							<ContextMenuItem onSelect={() => setPinOpen(true)}>Pin to Sidebar…</ContextMenuItem>
							<ContextMenuSeparator />
						</>
					)}
					{entry.isDir && (
						<>
							<ContextMenuItem onSelect={() => void openArtifactGrid(activeProjectId, entry.path)}>
								<Grid3x3 className="h-3.5 w-3.5" />
								Open as Artifact Grid
							</ContextMenuItem>
							<ContextMenuSeparator />
							<ContextMenuItem onSelect={() => openTerminalAt(terminalCwd)}>
								Open in Terminal
							</ContextMenuItem>
							<ContextMenuItem onSelect={() => openTerminalAt(terminalCwd, 'right')}>
								Open in Terminal to the Side
							</ContextMenuItem>
							<ContextMenuItem onSelect={() => openTerminalAt(terminalCwd, 'bottom')}>
								Open in Terminal Below
							</ContextMenuItem>
							<ContextMenuSeparator />
						</>
					)}
					<ContextMenuItem onSelect={copyPath}>Copy Path</ContextMenuItem>
					<ContextMenuItem onSelect={() => void writeClipboardText(entry.name).catch(() => {})}>
						Copy Name
					</ContextMenuItem>
					<ContextMenuSeparator />
					<ContextMenuItem onSelect={() => startRename()}>Rename…</ContextMenuItem>
					<ContextMenuItem variant="destructive" onSelect={() => void handleDelete()}>
						Move to Trash
					</ContextMenuItem>
				</ContextMenuContent>
			</ContextMenu>
			{pinOpen && (
				<PinArtifactDialog
					open
					onOpenChange={(o) => {
						if (!o) setPinOpen(false);
					}}
					path={entry.path}
					onPinned={() => setPinOpen(false)}
				/>
			)}
			{actionError && (
				<div
					role="alert"
					className="flex items-start gap-1 px-2 py-1 text-xs text-destructive"
					style={{ paddingLeft: `${Math.min(depth, 10) * 12 + 8}px` }}
				>
					<AlertCircle className="h-3 w-3 shrink-0 mt-0.5" />
					<span className="truncate" title={actionError}>
						{actionError}
					</span>
				</div>
			)}
			{entry.isDir && expanded && (
				<div>
					{error && (
						<div
							role="alert"
							className="flex items-start gap-1 px-2 py-1 text-xs text-destructive"
							style={{ paddingLeft: `${Math.min(depth + 1, 10) * 12 + 8}px` }}
						>
							<AlertCircle className="h-3 w-3 shrink-0 mt-0.5" />
							<span className="truncate" title={error}>
								{error}
							</span>
							<button
								type="button"
								onClick={() => collapse(entry.path)}
								className="ml-auto rounded px-1 text-muted-foreground hover:bg-background"
							>
								hide
							</button>
						</div>
					)}
					{children
						?.filter((child) => !filter || filter.has(child.path))
						.map((child) => (
							<TreeNode key={child.path} entry={child} depth={depth + 1} filter={filter} />
						))}
					{children && children.length === 0 && !error && (
						<div
							className="px-2 py-1 text-xs text-muted-foreground italic"
							style={{ paddingLeft: `${Math.min(depth + 1, 10) * 12 + 8}px` }}
						>
							empty
						</div>
					)}
				</div>
			)}
		</div>
	);
}

interface RootSectionProps {
	rootPath: string;
	/** This is the single expanded section in the accordion. */
	isOpen: boolean;
	/** Which edge of the shared scroller this header sticks to. Headers at/above
	 *  the open section stick to the top; headers below it stick to the bottom.
	 *  Because same-edge sticky siblings push each other out, only one stays
	 *  visibly stuck per edge — the rest scroll. */
	stickyEdge: 'top' | 'bottom';
}

function RootSection({ rootPath, isOpen, stickyEdge }: RootSectionProps) {
	const qc = useQueryClient();
	const showHidden = useFilesStore((s) => s.showHidden);
	const showIgnored = useFilesStore((s) => s.showIgnored);
	const storedQuery = useFilesStore((s) => s.queries[rootPath] ?? '');
	const setQuery = useFilesStore((s) => s.setQuery);
	const toggleRoot = useFilesStore((s) => s.toggleRoot);

	// Local input value tracks every keystroke; the store (and therefore the
	// search query key) is updated on a 200ms debounce so we don't fire one
	// `fs_search` per keystroke.
	const [inputValue, setInputValue] = useState(storedQuery);
	const debounceRef = useRef<number | null>(null);
	useEffect(() => {
		if (debounceRef.current !== null) window.clearTimeout(debounceRef.current);
		debounceRef.current = window.setTimeout(() => {
			setQuery(rootPath, inputValue.trim());
		}, 200);
		return () => {
			if (debounceRef.current !== null) window.clearTimeout(debounceRef.current);
		};
	}, [inputValue, rootPath, setQuery]);

	const selectSorted = useMemo(
		() => (list: FileEntry[]) => sortEntries(list, { showHidden, showIgnored }),
		[showHidden, showIgnored]
	);
	const rootQuery = useQuery({
		queryKey: queryKeys.fs.list(rootPath),
		queryFn: () => fsList(rootPath),
		enabled: isOpen,
		staleTime: 30_000,
		select: selectSorted,
	});

	const searchActive = storedQuery.length > 0;
	const searchQuery = useQuery({
		queryKey: queryKeys.fs.search(rootPath, storedQuery, showHidden, showIgnored),
		queryFn: () => fsSearch(rootPath, storedQuery, showHidden, showIgnored),
		enabled: searchActive && isOpen,
		staleTime: 30_000,
	});

	const matchSet = useMemo(() => {
		if (!searchActive || !searchQuery.data) return null;
		return buildMatchSet(searchQuery.data.matches, rootPath);
	}, [searchActive, searchQuery.data, rootPath]);

	const reload = useCallback(() => {
		void qc.invalidateQueries({ queryKey: queryKeys.fs.list(rootPath) });
		if (searchActive) {
			void qc.invalidateQueries({
				queryKey: queryKeys.fs.search(rootPath, storedQuery, showHidden, showIgnored),
			});
		}
	}, [qc, rootPath, searchActive, storedQuery, showHidden, showIgnored]);

	const clearSearch = useCallback(() => {
		setInputValue('');
		setQuery(rootPath, '');
	}, [rootPath, setQuery]);

	const displayName = rootPath.replace(/^.+\//, '') || rootPath;
	const error = rootQuery.error
		? rootQuery.error instanceof Error
			? rootQuery.error.message
			: String(rootQuery.error)
		: null;
	const entries = rootQuery.data;

	const visibleEntries = useMemo(() => {
		if (!entries) return entries;
		if (!matchSet) return entries;
		return entries.filter((e) => matchSet.has(e.path));
	}, [entries, matchSet]);

	const searchError = searchQuery.error
		? searchQuery.error instanceof Error
			? searchQuery.error.message
			: String(searchQuery.error)
		: null;

	return (
		<>
			{/* Header is a DIRECT child of the shared scroller so same-edge sticky
			    headers push each other out — only one stays stuck per edge. */}
			<div
				className={cn(
					'flex shrink-0 items-center justify-between border-b border-border bg-background px-2 py-1.5',
					'sticky z-10',
					stickyEdge === 'top' ? 'top-0' : 'bottom-0'
				)}
			>
				<button
					type="button"
					onClick={() => toggleRoot(rootPath)}
					className="flex min-w-0 flex-1 items-center gap-1 rounded px-1 py-0.5 text-left text-muted-foreground hover:bg-accent hover:text-accent-foreground"
					aria-expanded={isOpen}
					title={isOpen ? `Collapse ${rootPath}` : `Expand ${rootPath}`}
				>
					{isOpen ? (
						<ChevronDown className="h-3 w-3 shrink-0" />
					) : (
						<ChevronRight className="h-3 w-3 shrink-0" />
					)}
					<span className="truncate text-[10px] font-semibold uppercase tracking-wider">
						{displayName}
					</span>
					{!isOpen && searchActive && (
						<span
							className="ml-1 rounded bg-accent px-1 text-[9px] font-medium normal-case tracking-normal text-accent-foreground"
							title={`Search active: "${storedQuery}"`}
						>
							search
						</span>
					)}
				</button>
				{isOpen && (
					<button
						type="button"
						onClick={reload}
						className="rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-accent-foreground"
						title={`Reload ${rootPath}`}
						aria-label="Reload"
					>
						<RefreshCw className="h-3 w-3" />
					</button>
				)}
			</div>
			{isOpen && (
				<div className="border-b border-border">
					<div className="relative border-b border-border bg-background px-2 py-1">
						<Search className="pointer-events-none absolute left-3.5 top-1/2 h-3 w-3 -translate-y-1/2 text-muted-foreground" />
						<input
							type="text"
							value={inputValue}
							onChange={(e) => setInputValue(e.target.value)}
							onKeyDown={(e) => {
								if (e.key === 'Escape' && inputValue) {
									e.preventDefault();
									e.stopPropagation();
									clearSearch();
								}
							}}
							placeholder={`Search ${displayName}…`}
							className="h-6 w-full rounded border border-border bg-background pl-6 pr-6 text-xs text-foreground placeholder:text-muted-foreground outline-none focus:border-ring"
						/>
						{inputValue && (
							<button
								type="button"
								onClick={clearSearch}
								className="absolute right-3 top-1/2 -translate-y-1/2 rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground"
								title="Clear search"
								aria-label="Clear search"
							>
								<X className="h-3 w-3" />
							</button>
						)}
					</div>
					{searchActive && searchQuery.data?.truncated && (
						<div className="px-3 py-1 text-[11px] text-muted-foreground italic">
							Showing first {searchQuery.data.matches.length} matches — refine your query.
						</div>
					)}
					{searchError && (
						<div className="flex items-start gap-1 px-3 py-1 text-xs text-destructive">
							<AlertCircle className="h-3 w-3 shrink-0 mt-0.5" />
							<span className="break-all" title={searchError}>
								{searchError}
							</span>
						</div>
					)}
					{error && (
						<div className="flex items-start gap-1 px-3 py-1 text-xs text-destructive">
							<AlertCircle className="h-3 w-3 shrink-0 mt-0.5" />
							<span className="break-all" title={error}>
								{error}
							</span>
						</div>
					)}
					{!entries && !error && (
						<div className="px-3 py-1 text-xs text-muted-foreground italic">loading…</div>
					)}
					{searchActive && searchQuery.isFetching && !searchQuery.data && (
						<div className="px-3 py-1 text-xs text-muted-foreground italic">searching…</div>
					)}
					{searchActive && matchSet && matchSet.size === 0 && !searchQuery.isFetching && (
						<div className="px-3 py-1 text-xs text-muted-foreground italic">No matches.</div>
					)}
					{visibleEntries?.map((entry) => (
						<TreeNode key={entry.path} entry={entry} depth={0} filter={matchSet ?? undefined} />
					))}
				</div>
			)}
		</>
	);
}

export function FilesMode() {
	const fileRoots = useShellStore((s) => s.fileRoots);
	const hydrated = useFilesStore((s) => s.hydrated);
	const hydrate = useFilesStore((s) => s.hydrate);
	const expandedRoot = useFilesStore((s) => s.expandedRoot);
	const setExpandedRoot = useFilesStore((s) => s.setExpandedRoot);
	const reveal = useFilesStore((s) => s.reveal);
	const storedScrollTop = useFilesStore((s) => s.scrollTop);
	const setScrollTop = useFilesStore((s) => s.setScrollTop);
	const showHidden = useFilesStore((s) => s.showHidden);
	const showIgnored = useFilesStore((s) => s.showIgnored);
	const setShowHidden = useFilesStore((s) => s.setShowHidden);
	const setShowIgnored = useFilesStore((s) => s.setShowIgnored);
	const toggleShowHidden = useFilesStore((s) => s.toggleShowHidden);
	const scrollerRef = useRef<HTMLDivElement | null>(null);

	// Index of the open root; headers at/above it stick to the top, those below
	// stick to the bottom. `-1` (none open) → everything sticks to the top.
	const openIndex = expandedRoot ? fileRoots.indexOf(expandedRoot) : -1;

	useEffect(() => {
		void hydrate();
	}, [hydrate]);

	// First launch: open the first root so the accordion always starts with one
	// section expanded. Runs once, only after roots exist and hydration loaded
	// any persisted `expandedRoot` — so it never fights a manual collapse-to-none
	// or a persisted choice from a previous session.
	const didInitOpen = useRef(false);
	useEffect(() => {
		if (!hydrated || didInitOpen.current || fileRoots.length === 0) return;
		didInitOpen.current = true;
		if (expandedRoot === null) setExpandedRoot(fileRoots[0]);
	}, [hydrated, expandedRoot, fileRoots, setExpandedRoot]);

	// Reveal-on-open: when a file is opened into a pane, open its root, expand
	// the ancestor dirs leading to it, and select it (TreeNode scrolls itself
	// into view). This effect lives in FilesMode, so it only runs while the
	// Files sidebar is mounted (i.e. "if the Files pane is open"). We seed the
	// last-handled nonce with whatever exists at mount so opening the pane does
	// not retroactively jump to an already-open file — only genuine opens after
	// mount trigger a reveal.
	const revealRequest = usePaneStore((s) => s.revealRequest);
	const lastRevealNonce = useRef<number | null>(
		usePaneStore.getState().revealRequest?.nonce ?? null
	);
	useEffect(() => {
		if (!revealRequest || lastRevealNonce.current === revealRequest.nonce) return;
		lastRevealNonce.current = revealRequest.nonce;
		const { path } = revealRequest;
		const root = fileRoots.find(
			(r) => path === r || path.startsWith(r.endsWith('/') ? r : `${r}/`)
		);
		if (!root) return;
		// Ancestor dirs strictly between the root and the file. The root section's
		// expansion is governed by `expandedRoot`, and its direct children render
		// whenever it's open — so we only need the intermediate dirs in `expanded`.
		const ancestors: string[] = [];
		let cur = parentOf(path);
		while (cur.length > root.length && cur.startsWith(root)) {
			ancestors.push(cur);
			cur = parentOf(cur);
		}
		reveal(root, ancestors, path);
	}, [revealRequest, fileRoots, reveal]);

	// Cmd+. (Mac) / Ctrl+. (Linux/Windows) → toggle hidden files. Matches the
	// Finder convention. Bare `.` (no modifier) is left alone so users can
	// still type into rename inputs and search boxes. Scoped to keypresses
	// outside text inputs, same gating as the activity-bar shortcuts.
	useEffect(() => {
		function onKey(e: KeyboardEvent) {
			const mod = e.metaKey || e.ctrlKey;
			if (!mod || e.shiftKey || e.altKey) return;
			if (e.key !== '.') return;
			const target = e.target as HTMLElement | null;
			if (target?.matches('input, textarea, [contenteditable="true"]')) return;
			e.preventDefault();
			toggleShowHidden();
		}
		window.addEventListener('keydown', onKey);
		return () => window.removeEventListener('keydown', onKey);
	}, [toggleShowHidden]);

	// Restore the tree scroll once on hydrate. Re-apply after a beat so
	// late-resolving queries that grow the tree don't clobber it. Reveal-on-open
	// scroll-into-view (TreeNode) fires later as a passive effect, so it wins.
	// biome-ignore lint/correctness/useExhaustiveDependencies: storedScrollTop is read fresh from the closure but we deliberately don't re-run on every scroll-save.
	useLayoutEffect(() => {
		if (!hydrated || !scrollerRef.current) return;
		const el = scrollerRef.current;
		el.scrollTop = storedScrollTop;
		const t = window.setTimeout(() => {
			el.scrollTop = storedScrollTop;
		}, 200);
		return () => window.clearTimeout(t);
	}, [hydrated]);

	const scrollSaveRef = useRef<number | null>(null);
	const onScroll = useCallback(() => {
		if (!scrollerRef.current) return;
		const top = scrollerRef.current.scrollTop;
		if (scrollSaveRef.current !== null) window.clearTimeout(scrollSaveRef.current);
		scrollSaveRef.current = window.setTimeout(() => setScrollTop(top), 150);
	}, [setScrollTop]);

	return (
		<div className="flex h-full flex-col">
			<div className="flex items-center justify-between border-b border-border px-3 py-1.5">
				<span className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
					Files
				</span>
				<DropdownMenu>
					<DropdownMenuTrigger asChild>
						<button
							type="button"
							className="rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-accent-foreground"
							title="View options"
							aria-label="View options"
						>
							<MoreHorizontal className="h-3.5 w-3.5" />
						</button>
					</DropdownMenuTrigger>
					<DropdownMenuContent align="end" className="w-56">
						<DropdownMenuCheckboxItem
							checked={showHidden}
							onCheckedChange={(v) => setShowHidden(Boolean(v))}
						>
							Show hidden files
							<span className="ml-auto text-[10px] text-muted-foreground">⌘.</span>
						</DropdownMenuCheckboxItem>
						<DropdownMenuCheckboxItem
							checked={showIgnored}
							onCheckedChange={(v) => setShowIgnored(Boolean(v))}
						>
							Show ignored folders
						</DropdownMenuCheckboxItem>
					</DropdownMenuContent>
				</DropdownMenu>
			</div>
			<div ref={scrollerRef} onScroll={onScroll} className="min-h-0 flex-1 overflow-auto">
				{fileRoots.length === 0 && (
					<div className="p-4 text-xs text-muted-foreground">
						No file roots configured. Add one from{' '}
						<span className="font-medium text-foreground">Settings</span>.
					</div>
				)}
				{fileRoots.map((root, i) => (
					<RootSection
						key={root}
						rootPath={root}
						isOpen={expandedRoot === root}
						stickyEdge={openIndex === -1 || i <= openIndex ? 'top' : 'bottom'}
					/>
				))}
			</div>
		</div>
	);
}
