import { useEffect, useState, useCallback, useMemo } from 'react';
import type { AccessStatus } from '@/lib/access/client';
import {
	askForFoldersCopy,
	cannotEditRootsReason,
	isAbsoluteServerPath,
	isPermissionError,
} from '@/lib/fs-allowlist';
import { useDialogStore } from '@/lib/transport/dialog-store';
import { getTransport } from '@/lib/transport';

interface FsEntry {
	name: string;
	is_dir: boolean;
	path: string;
	size?: number;
}

export function FilepickerModal() {
	const activeRequest = useDialogStore((s) => s.activeRequest);
	const closeDialog = useDialogStore((s) => s.closeDialog);

	const isPicker = activeRequest?.type === 'open' || activeRequest?.type === 'save';
	const options = activeRequest?.options || {};

	const [currentDir, setCurrentDir] = useState<string>('.');
	const [entries, setEntries] = useState<FsEntry[]>([]);
	const [query, setQuery] = useState<string>('');
	const [selectedIndex, setSelectedIndex] = useState<number>(0);
	const [loading, setLoading] = useState<boolean>(false);
	const [error, setError] = useState<string | null>(null);
	// The folders the daemon will let this session list (its fs allowlist).
	const [roots, setRoots] = useState<string[]>([]);
	// Whether `roots` holds the daemon's answer yet (vs. the initial empty list).
	const [rootsLoaded, setRootsLoaded] = useState<boolean>(false);
	// The allowlist came back empty: nothing can be listed until a folder is added.
	const [noRoots, setNoRoots] = useState<boolean>(false);

	const fetchRoots = useCallback(async (): Promise<string[]> => {
		try {
			const r = await getTransport().invoke<string[]>('fs_roots_list', {});
			const list = Array.isArray(r) ? r : [];
			setRoots(list);
			setRootsLoaded(true);
			return list;
		} catch {
			return [];
		}
	}, []);

	// Initial directory load. With no `defaultPath` the picker used to start at `.`, which on
	// a headless daemon is the daemon's own working directory, never on the allowlist, so it
	// opened on an error with no way out — and "Select Folder" then committed `.` as a project.
	// Start in the first allowed folder; with none, offer to add one and never fall back to `.`.
	useEffect(() => {
		setQuery('');
		setSelectedIndex(0);
		setNoRoots(false);
		if (options.defaultPath) {
			setCurrentDir(options.defaultPath);
			return;
		}
		let cancelled = false;
		setCurrentDir(''); // resolving; the listing effect waits for a real path
		void (async () => {
			const list = await fetchRoots();
			if (cancelled) return;
			if (list.length > 0) setCurrentDir(list[0]);
			else {
				setEntries([]);
				setNoRoots(true);
			}
		})();
		return () => {
			cancelled = true;
		};
	}, [activeRequest?.id, options.defaultPath, fetchRoots]);

	// A folder was added from the empty state: open it.
	const onRootAdded = useCallback((list: string[], added: string) => {
		setRoots(list);
		setNoRoots(false);
		setCurrentDir(added);
	}, []);

	// Fetch directory contents
	const loadDirectory = useCallback(async (dirPath: string) => {
		setLoading(true);
		setError(null);
		try {
			const transport = getTransport();
			const result = await transport.invoke<FsEntry[]>('fs_list', { path: dirPath });
			if (Array.isArray(result)) {
				// Sort directories first, then files
				const sorted = [...result].sort((a, b) => {
					if (a.is_dir && !b.is_dir) return -1;
					if (!a.is_dir && b.is_dir) return 1;
					return a.name.localeCompare(b.name);
				});
				setEntries(sorted);
				setCurrentDir(dirPath);
			}
		} catch (err) {
			// Surfaced, not just logged: swallowing this renders an empty
			// picker that is indistinguishable from an empty directory, and
			// the user has no way to tell that the listing failed.
			console.warn('[filepicker-modal] failed to list dir:', err);
			setEntries([]);
			const message = err instanceof Error ? err.message : String(err);
			setError(message);
			// Outside the allowlist: learn where we may go so the error can offer it.
			if (message.includes('outside allowlist')) void fetchRoots();
		} finally {
			setLoading(false);
		}
	}, [fetchRoots]);

	useEffect(() => {
		if (isPicker && currentDir) {
			loadDirectory(currentDir);
		}
	}, [isPicker, currentDir, loadDirectory]);

	// Filter entries based on query
	const filteredEntries = useMemo(() => {
		if (!query.trim()) return entries;
		const q = query.toLowerCase();
		return entries.filter((e) => e.name.toLowerCase().includes(q));
	}, [entries, query]);

	// Reset selected index when filter changes
	useEffect(() => {
		setSelectedIndex(0);
	}, [query]);

	const hostName =
		typeof window !== 'undefined' ? window.location.hostname || 'ikenga.host' : 'ikenga.host';

	// Only a folder the daemon actually listed can be picked: never `.` or a relative path
	// (it would resolve against the daemon's working directory), and never one that failed.
	const canPickDir =
		!noRoots && !loading && !error && currentDir !== '' && isAbsoluteServerPath(currentDir);

	const handleConfirm = useCallback(() => {
		if (options.directory) {
			if (canPickDir) closeDialog(currentDir);
			return;
		}
		const selected = filteredEntries[selectedIndex];
		if (!selected) {
			if (canPickDir) closeDialog(currentDir);
			return;
		}
		if (selected.is_dir) {
			loadDirectory(selected.path);
		} else {
			closeDialog(selected.path);
		}
	}, [
		options.directory,
		canPickDir,
		currentDir,
		filteredEntries,
		selectedIndex,
		closeDialog,
		loadDirectory,
	]);

	const handleKeyDown = useCallback(
		(e: React.KeyboardEvent) => {
			if (e.key === 'Escape') {
				e.preventDefault();
				closeDialog(null);
			} else if (e.key === 'ArrowDown') {
				e.preventDefault();
				setSelectedIndex((prev) => Math.min(prev + 1, Math.max(0, filteredEntries.length - 1)));
			} else if (e.key === 'ArrowUp') {
				e.preventDefault();
				setSelectedIndex((prev) => Math.max(prev - 1, 0));
			} else if (e.key === 'Enter') {
				e.preventDefault();
				handleConfirm();
			} else if (e.key === 'Tab' || e.key === 'ArrowRight') {
				const selected = filteredEntries[selectedIndex];
				if (selected && selected.is_dir) {
					e.preventDefault();
					loadDirectory(selected.path);
				}
			}
		},
		[filteredEntries, selectedIndex, closeDialog, handleConfirm, loadDirectory]
	);

	if (!isPicker) return null;

	const parts = currentDir.split('/').filter(Boolean);

	return (
		<div
			className="fixed inset-0 z-50 grid place-items-center bg-black/60 p-6 backdrop-blur-xs"
			onClick={() => closeDialog(null)}
		>
			<div
				className="w-full max-w-[600px] overflow-hidden rounded-xl border border-[var(--border-strong)] bg-[var(--bg-surface)] text-[var(--fg)] shadow-2xl"
				onClick={(e) => e.stopPropagation()}
				onKeyDown={handleKeyDown}
			>
				{/* Query bar */}
				<div className="flex items-center gap-2.5 border-b border-[var(--border-soft)] px-5 py-4">
					<span className="text-[var(--fg-faint)]">⌕</span>
					<input
						type="text"
						value={query}
						onChange={(e) => setQuery(e.target.value)}
						placeholder="Search path or name..."
						className="flex-1 bg-transparent font-mono text-[var(--text-body-lg)] text-[var(--fg)] outline-none placeholder:text-[var(--fg-faint)]"
						autoFocus
						spellCheck={false}
					/>
					<kbd className="rounded border border-[var(--border)] px-1.5 py-0.5 font-mono text-[10px] text-[var(--fg-faint)]">
						esc
					</kbd>
				</div>

				{/* Breadcrumb */}
				<div className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-5 py-2 font-mono text-[11px] text-[var(--fg-faint)]">
					/<span className="text-[var(--fg-muted)]">{parts.join('/')}</span>/
				</div>

				{/* Directory list */}
				<div className="max-h-[280px] overflow-y-auto py-2">
					{noRoots ? (
						<AddFolderPanel known={roots} onAdded={onRootAdded} />
					) : loading ? (
						<div className="p-4 text-center font-mono text-[12px] text-[var(--fg-faint)]">
							Loading...
						</div>
					) : error && rootsLoaded && roots.length === 0 ? (
						<div className="p-4 text-center font-mono text-[12px]" data-testid="filepicker-error">
							<div className="text-[var(--danger)]">Could not list this directory — {error}</div>
							<AddFolderPanel known={roots} onAdded={onRootAdded} />
						</div>
					) : error ? (
						<div
							className="p-4 text-center font-mono text-[12px] text-[var(--danger)]"
							data-testid="filepicker-error"
						>
							Could not list this directory — {error}
							{roots.length > 0 && (
								<div className="mt-3 flex flex-col items-center gap-1.5">
									<span className="text-[var(--fg-faint)]">Folders you can open:</span>
									{roots.map((r) => (
										<button
											key={r}
											type="button"
											data-testid="filepicker-root"
											onClick={() => loadDirectory(r)}
											className="cursor-pointer rounded border border-[var(--border)] px-2.5 py-1 text-[var(--fg)] hover:bg-[var(--bg-raised)]"
										>
											{r}
										</button>
									))}
								</div>
							)}
						</div>
					) : filteredEntries.length === 0 ? (
						<div className="p-4 text-center font-mono text-[12px] text-[var(--fg-faint)]">
							No matches
						</div>
					) : (
						filteredEntries.map((item, idx) => {
							const isSelected = idx === selectedIndex;
							return (
								<div
									key={item.path}
									className={`flex items-center gap-2.5 px-5 py-1.5 text-[13px] cursor-pointer ${
										isSelected
											? 'bg-[var(--primary-soft)] shadow-[inset_2px_0_0_var(--primary)] text-[var(--fg)] font-medium'
											: 'hover:bg-[var(--bg-raised)] text-[var(--fg-muted)]'
									}`}
									onClick={() => {
										setSelectedIndex(idx);
										if (item.is_dir) {
											loadDirectory(item.path);
										} else {
											closeDialog(item.path);
										}
									}}
								>
									<span className="w-3.5 text-center text-[var(--fg-faint)]">
										{item.is_dir ? '▸' : '▪'}
									</span>
									<span className="flex-1 font-mono text-[12px]">{item.name}</span>
									<span className="font-mono text-[10px] text-[var(--fg-faint)]">
										{item.is_dir
											? 'dir'
											: item.size
												? `${(item.size / 1024).toFixed(1)} KB`
												: 'file'}
									</span>
								</div>
							);
						})
					)}
				</div>

				{/* Footer */}
				<div className="flex items-center gap-3 border-t border-[var(--border-soft)] px-5 py-3 text-[11px] text-[var(--fg-faint)]">
					<span className="flex items-center gap-1.5 font-mono text-[var(--info)]">
						<span className="inline-block h-2 w-2 rounded-full bg-[var(--info)]" />
						{hostName}
					</span>
					<span>↑↓ navigate · ↵ open · ⇥ into folder</span>
					<button
						type="button"
						onClick={handleConfirm}
						disabled={options.directory ? !canPickDir : false}
						className="ml-auto rounded-md bg-[var(--primary)] px-4 py-1.5 font-semibold text-[13px] text-[var(--primary-fg)] hover:opacity-90 cursor-pointer disabled:cursor-not-allowed disabled:opacity-40"
					>
						{options.directory ? 'Select Folder' : 'Open'}
					</button>
				</div>
			</div>
		</div>
	);
}

/**
 * The picker's empty state: this server lets the session open no folders yet. Add one by its
 * full path on the server (the picker can't browse what it may not list), or — when this
 * caller can't change the list — say who can.
 */
function AddFolderPanel({
	known,
	onAdded,
}: {
	known: string[];
	onAdded: (list: string[], added: string) => void;
}) {
	const [status, setStatus] = useState<AccessStatus | null>(null);
	const [refused, setRefused] = useState<string | null>(null);
	const [path, setPath] = useState('');
	const [busy, setBusy] = useState(false);
	const [addError, setAddError] = useState<string | null>(null);

	useEffect(() => {
		let cancelled = false;
		getTransport()
			.invoke<AccessStatus>('access_status', {})
			.then((s) => {
				if (!cancelled && s && Array.isArray(s.caps)) setStatus(s);
			})
			.catch(() => {
				// Unknown: let the server decide when the caller tries.
			});
		return () => {
			cancelled = true;
		};
	}, []);

	const reason = refused ?? cannotEditRootsReason(status);

	async function add() {
		const trimmed = path.trim();
		if (!isAbsoluteServerPath(trimmed)) {
			setAddError("Give the folder's full path on the server, starting with /.");
			return;
		}
		setBusy(true);
		setAddError(null);
		try {
			const list = await getTransport().invoke<string[]>('fs_roots_add', { path: trimmed });
			const next = Array.isArray(list) ? list : [];
			// The server stores the folder canonicalized, so open the entry that is new
			// rather than the spelling typed here.
			const added =
				next.find((r) => !known.includes(r)) ?? (next.includes(trimmed) ? trimmed : next[0]);
			onAdded(next, added ?? trimmed);
		} catch (err) {
			const message = err instanceof Error ? err.message : String(err);
			if (isPermissionError(message)) setRefused(message);
			else setAddError(message);
		} finally {
			setBusy(false);
		}
	}

	if (reason) {
		return (
			<div
				className="p-4 text-center text-[12px] text-[var(--fg-muted)]"
				data-testid="filepicker-ask"
			>
				<p className="font-medium text-[var(--fg)]">
					This server doesn't let you open any folders yet.
				</p>
				<p className="mt-1">{askForFoldersCopy(status)}</p>
				<p className="mt-1 text-[11px] text-[var(--fg-faint)]">{reason}</p>
			</div>
		);
	}
	return (
		<div className="p-4 text-[12px] text-[var(--fg-muted)]" data-testid="filepicker-add-folder">
			<p className="text-center font-medium text-[var(--fg)]">
				This server doesn't let you open any folders yet.
			</p>
			<p className="mt-1 text-center">Add a folder by its full path on the server.</p>
			<div className="mt-3 flex items-center gap-2">
				<input
					type="text"
					value={path}
					onChange={(e) => setPath(e.target.value)}
					onKeyDown={(e) => {
						// The picker's own Enter / arrows must not fire while typing here.
						e.stopPropagation();
						if (e.key === 'Enter') void add();
					}}
					placeholder="/home/you/projects"
					aria-label="Folder to add"
					spellCheck={false}
					className="flex-1 rounded border border-[var(--border)] bg-transparent px-2 py-1 font-mono text-[12px] text-[var(--fg)] outline-none"
				/>
				<button
					type="button"
					onClick={() => void add()}
					disabled={busy || !path.trim()}
					className="cursor-pointer rounded border border-[var(--border)] px-2.5 py-1 text-[var(--fg)] hover:bg-[var(--bg-raised)] disabled:cursor-not-allowed disabled:opacity-40"
				>
					Add folder
				</button>
			</div>
			{addError && (
				<p className="mt-2 font-mono text-[11px] text-[var(--danger)]" role="alert">
					{addError}
				</p>
			)}
		</div>
	);
}
