import { useEffect, useState, useCallback, useMemo } from 'react';
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

	// Initial directory load
	useEffect(() => {
		if (options.defaultPath) {
			setCurrentDir(options.defaultPath);
		} else {
			setCurrentDir('.');
		}
		setQuery('');
		setSelectedIndex(0);
	}, [activeRequest?.id, options.defaultPath]);

	// Fetch directory contents
	const loadDirectory = useCallback(async (dirPath: string) => {
		setLoading(true);
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
			console.warn('[filepicker-modal] failed to list dir:', err);
		} finally {
			setLoading(false);
		}
	}, []);

	useEffect(() => {
		if (isPicker) {
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

	const hostName = typeof window !== 'undefined' ? window.location.hostname || 'ikenga.host' : 'ikenga.host';

	const handleConfirm = useCallback(() => {
		if (options.directory) {
			closeDialog(currentDir);
			return;
		}
		const selected = filteredEntries[selectedIndex];
		if (!selected) {
			closeDialog(currentDir);
			return;
		}
		if (selected.is_dir) {
			loadDirectory(selected.path);
		} else {
			closeDialog(selected.path);
		}
	}, [options.directory, currentDir, filteredEntries, selectedIndex, closeDialog, loadDirectory]);

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
					{loading ? (
						<div className="p-4 text-center font-mono text-[12px] text-[var(--fg-faint)]">Loading...</div>
					) : filteredEntries.length === 0 ? (
						<div className="p-4 text-center font-mono text-[12px] text-[var(--fg-faint)]">No matches</div>
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
									<span className="w-3.5 text-center text-[var(--fg-faint)]">{item.is_dir ? '▸' : '▪'}</span>
									<span className="flex-1 font-mono text-[12px]">{item.name}</span>
									<span className="font-mono text-[10px] text-[var(--fg-faint)]">
										{item.is_dir ? 'dir' : item.size ? `${(item.size / 1024).toFixed(1)} KB` : 'file'}
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
						className="ml-auto rounded-md bg-[var(--primary)] px-4 py-1.5 font-semibold text-[13px] text-[var(--primary-fg)] hover:opacity-90 cursor-pointer"
					>
						{options.directory ? 'Select Folder' : 'Open'}
					</button>
				</div>
			</div>
		</div>
	);
}
