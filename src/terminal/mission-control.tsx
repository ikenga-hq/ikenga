import { Cpu, DollarSign, LayoutGrid, Play, Send, Terminal } from 'lucide-react';
import { useState } from 'react';
import { Button } from '@/components/ui/button';
import { useTerminalStore } from '@/terminal/session-store';

export function MissionControl() {
	const tabs = useTerminalStore((s) => s.tabs);
	const setActive = useTerminalStore((s) => s.setActive);
	const [globalPrompt, setGlobalPrompt] = useState('');
	const [targetSession, setTargetSession] = useState<string>('all');

	const handleDispatch = () => {
		if (!globalPrompt.trim()) return;
		console.log(`[MissionControl] Dispatching prompt to ${targetSession}:`, globalPrompt);
		// Reset input after dispatch
		setGlobalPrompt('');
	};

	if (tabs.length === 0) {
		return (
			<div className="flex h-full flex-col items-center justify-center p-6 text-center text-xs text-muted-foreground font-mono select-none">
				<LayoutGrid className="mb-2 h-6 w-6 text-muted-foreground/40" />
				<p className="font-semibold text-zinc-300">Multi-Session Mission Control</p>
				<p className="mt-1 text-[11px]">No active terminal sessions open.</p>
			</div>
		);
	}

	return (
		<div className="flex h-full flex-col bg-zinc-950 p-4 text-xs font-mono text-zinc-200 select-none overflow-y-auto">
			{/* Top Header & Dispatcher */}
			<div className="flex items-center justify-between border-b border-border/40 pb-3">
				<div className="flex items-center gap-2 font-semibold text-purple-400">
					<LayoutGrid className="h-4 w-4" />
					<span>Mission Control ({tabs.length} sessions active)</span>
				</div>

				<div className="flex items-center gap-2">
					<select
						value={targetSession}
						onChange={(e) => setTargetSession(e.target.value)}
						className="h-7 rounded bg-zinc-900 border border-zinc-700 px-2 text-[11px] text-zinc-200 focus:outline-none"
					>
						<option value="all">All Sessions</option>
						{tabs.map((t) => (
							<option key={t.id} value={t.id}>
								{t.title} ({t.id.slice(0, 6)})
							</option>
						))}
					</select>

					<input
						type="text"
						placeholder="Dispatch prompt across sessions..."
						value={globalPrompt}
						onChange={(e) => setGlobalPrompt(e.target.value)}
						onKeyDown={(e) => e.key === 'Enter' && handleDispatch()}
						className="h-7 w-64 rounded bg-zinc-900 border border-zinc-700 px-2.5 text-[11px] text-zinc-200 focus:outline-none"
					/>

					<Button size="sm" onClick={handleDispatch} className="h-7 px-3 bg-purple-600 hover:bg-purple-500 text-white text-[11px]">
						<Send className="mr-1 h-3 w-3" /> Dispatch
					</Button>
				</div>
			</div>

			{/* Session Grid */}
			<div className="mt-4 grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-3">
				{tabs.map((t) => (
					<div
						key={t.id}
						onClick={() => setActive(t.id)}
						className="group flex flex-col justify-between rounded-lg border border-zinc-800 bg-zinc-900/60 p-3 hover:border-purple-600/60 transition-all cursor-pointer"
					>
						<div>
							<div className="flex items-center justify-between">
								<div className="flex items-center gap-1.5 font-semibold text-zinc-100">
									<Terminal className="h-3.5 w-3.5 text-sky-400" />
									<span className="truncate max-w-[150px]">{t.title}</span>
								</div>

								<span
									className={`rounded px-1.5 py-0.5 text-[9px] font-bold uppercase ${
										t.status === 'running' ? 'bg-emerald-950 text-emerald-400 border border-emerald-800/40' : 'bg-zinc-800 text-zinc-400'
									}`}
								>
									{t.status}
								</span>
							</div>

							<div className="mt-2 space-y-1 text-[10px] text-zinc-400">
								<div className="flex items-center gap-1">
									<Cpu className="h-3 w-3 text-purple-400" />
									<span>Claude 3.5 Sonnet</span>
								</div>

								<div className="flex items-center gap-1 truncate" title={t.spec.cwd}>
									<span className="text-zinc-500">CWD:</span>
									<span className="truncate">{t.spec.cwd}</span>
								</div>
							</div>
						</div>

						<div className="mt-3 flex items-center justify-between border-t border-zinc-800/60 pt-2 text-[10px]">
							<div className="flex items-center gap-1 text-emerald-400 font-semibold">
								<DollarSign className="h-3 w-3" />
								<span>$0.024</span>
							</div>

							<div className="flex items-center gap-1 text-purple-400 group-hover:underline">
								<span>Focus Tab</span>
								<Play className="h-2.5 w-2.5" />
							</div>
						</div>
					</div>
				))}
			</div>
		</div>
	);
}
