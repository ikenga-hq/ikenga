import { listen } from '@tauri-apps/api/event';
import { Check, FileCode, GitBranch, GitCommit, RotateCcw } from 'lucide-react';
import { useEffect, useState } from 'react';
import { Button } from '@/components/ui/button';

export interface GitLedgerItem {
	path: string;
	status: 'M' | 'A' | 'D' | 'U';
	diff?: string;
	staged: boolean;
}

export function GitLedger({ sessionId }: { sessionId: string }) {
	const [touchedFiles, setTouchedFiles] = useState<Map<string, GitLedgerItem>>(new Map());
	const [selectedFile, setSelectedFile] = useState<string | null>(null);
	const [commitMsg, setCommitMsg] = useState('feat: changes from turn');
	const [isCommitting, setIsCommitting] = useState(false);

	useEffect(() => {
		let unlisten: (() => void) | undefined;

		listen<{ hook_event_name?: string; session_id?: string; tool_name?: string; tool_input?: { path?: string; target_file?: string } }>(
			'hooks://event',
			(event) => {
				const p = event.payload;
				if (!p || p.hook_event_name !== 'PostToolUse') return;
				if (sessionId && p.session_id && p.session_id !== sessionId) return;

				const filePath = p.tool_input?.path || p.tool_input?.target_file;
				if (filePath) {
					setTouchedFiles((prev) => {
						const next = new Map(prev);
						if (!next.has(filePath)) {
							next.set(filePath, { path: filePath, status: 'M', staged: false });
						}
						return next;
					});
				}
			}
		)
			.then((fn) => {
				unlisten = fn;
			})
			.catch(() => {});

		return () => {
			if (unlisten) unlisten();
		};
	}, [sessionId]);

	const toggleStage = (path: string) => {
		setTouchedFiles((prev) => {
			const next = new Map(prev);
			const item = next.get(path);
			if (item) {
				next.set(path, { ...item, staged: !item.staged });
			}
			return next;
		});
	};

	const revertFile = (path: string) => {
		setTouchedFiles((prev) => {
			const next = new Map(prev);
			next.delete(path);
			if (selectedFile === path) setSelectedFile(null);
			return next;
		});
	};

	const filesArray = Array.from(touchedFiles.values());

	if (filesArray.length === 0) {
		return (
			<div className="flex h-full flex-col items-center justify-center p-6 text-center text-xs text-muted-foreground font-mono select-none">
				<GitBranch className="mb-2 h-6 w-6 text-muted-foreground/40" />
				<p className="font-semibold text-zinc-300">Per-Turn Git Change Ledger</p>
				<p className="mt-1 text-[11px]">No files modified in current turn yet.</p>
			</div>
		);
	}

	return (
		<div className="flex h-full flex-col bg-zinc-950 p-3 text-xs font-mono text-zinc-200 select-none">
			{/* Top Bar: Summary & Commit */}
			<div className="flex items-center justify-between border-b border-border/40 pb-2.5">
				<div className="flex items-center gap-1.5 font-semibold text-sky-400">
					<GitBranch className="h-3.5 w-3.5" />
					<span>Ledger ({filesArray.length} files touched)</span>
				</div>

				<div className="flex items-center gap-1.5">
					<input
						type="text"
						value={commitMsg}
						onChange={(e) => setCommitMsg(e.target.value)}
						className="h-6 w-44 rounded bg-zinc-900 border border-zinc-700 px-2 text-[10px] text-zinc-200 focus:outline-none"
					/>
					<Button
						size="sm"
						className="h-6 px-2 text-[10px] bg-emerald-600 hover:bg-emerald-500 text-white"
						disabled={isCommitting}
						onClick={() => {
							setIsCommitting(true);
							setTimeout(() => {
								setIsCommitting(false);
								setTouchedFiles(new Map());
							}, 600);
						}}
					>
						<GitCommit className="mr-1 h-3 w-3" />
						Commit Turn
					</Button>
				</div>
			</div>

			{/* File List */}
			<div className="mt-2 flex-1 overflow-y-auto space-y-1">
				{filesArray.map((item) => (
					<div
						key={item.path}
						className={`flex items-center justify-between rounded p-2 border ${
							selectedFile === item.path ? 'bg-zinc-900 border-sky-800/60' : 'bg-zinc-900/40 border-zinc-800/40'
						}`}
					>
						<div
							className="flex items-center gap-2 cursor-pointer truncate"
							onClick={() => setSelectedFile(item.path === selectedFile ? null : item.path)}
						>
							<FileCode className="h-3.5 w-3.5 text-zinc-400 shrink-0" />
							<span className="truncate text-[11px] text-zinc-200">{item.path}</span>
							<span className="rounded bg-sky-950 px-1 text-[9px] text-sky-400 font-bold">{item.status}</span>
						</div>

						<div className="flex items-center gap-1 shrink-0">
							<Button
								size="sm"
								variant="ghost"
								onClick={() => toggleStage(item.path)}
								className={`h-5 px-1.5 text-[10px] ${item.staged ? 'text-emerald-400' : 'text-zinc-400'}`}
							>
								<Check className="mr-0.5 h-3 w-3" />
								{item.staged ? 'Staged' : 'Stage'}
							</Button>

							<Button
								size="sm"
								variant="ghost"
								onClick={() => revertFile(item.path)}
								className="h-5 px-1.5 text-[10px] text-rose-400 hover:text-rose-300"
							>
								<RotateCcw className="mr-0.5 h-3 w-3" /> Revert
							</Button>
						</div>
					</div>
				))}
			</div>
		</div>
	);
}
