import { Activity, AlertTriangle, Cpu, DollarSign, Gauge, ShieldAlert } from 'lucide-react';
import { useEffect, useState } from 'react';
import { iykeFetch } from '@/lib/iyke/client';
import { listen } from '@/lib/tauri-cmd';

export interface StatuslineSnapshot {
	ikenga_terminal_id?: string;
	session_id?: string;
	cwd?: string;
	transcript_path?: string;
	model?: { id?: string; display_name?: string };
	cost?: {
		total_cost_usd?: number;
		total_duration_ms?: number;
		total_api_duration_ms?: number;
		total_lines_added?: number;
		total_lines_removed?: number;
	};
	context_window?: {
		total_input_tokens?: number;
		total_output_tokens?: number;
		context_window_size?: number;
		used_percentage?: number;
		remaining_percentage?: number;
	};
	exceeds_200k_tokens?: boolean;
	effort?: { level?: string };
	thinking?: { enabled?: boolean };
	rate_limits?: {
		five_hour?: { used_percentage?: number; resets_at?: number };
		seven_day?: { used_percentage?: number; resets_at?: number };
	};
}

export function CostHud({ sessionId }: { sessionId: string }) {
	const [snapshot, setSnapshot] = useState<StatuslineSnapshot | null>(null);

	useEffect(() => {
		// Initial fetch from backend snapshot REST endpoint if available.
		// Live endpoint + bearer token — see the note in tool-call-feed.tsx.
		// The endpoint now returns a per-terminal map; we pick this terminal's
		// snapshot and ignore events from siblings.
		iykeFetch('/iyke/statusline/snapshot')
			.then((res) => (res.ok ? res.json() : null))
			.then((data: Record<string, StatuslineSnapshot> | null) => {
				if (data) {
					setSnapshot(data[sessionId] ?? null);
				}
			})
			.catch(() => {});

		// Subscribe to real-time statusline updates over Tauri event bus
		let unlisten: (() => void) | undefined;
		listen<StatuslineSnapshot>('statusline://snapshot', (event) => {
			if (event.payload?.ikenga_terminal_id === sessionId) {
				setSnapshot(event.payload);
			}
		})
			.then((fn) => {
				unlisten = fn;
			})
			.catch(() => {});

		return () => {
			if (unlisten) unlisten();
		};
	}, []);

	if (!snapshot) {
		return (
			<div className="flex h-7 items-center justify-between border-b border-border/40 bg-zinc-950/80 px-3 text-[11px] text-muted-foreground backdrop-blur font-mono select-none">
				<div className="flex items-center gap-1.5">
					<Gauge className="h-3 w-3 text-muted-foreground/60" />
					<span>HUD: listening for statusline telemetry...</span>
				</div>
			</div>
		);
	}

	const usedPct = Math.round(snapshot.context_window?.used_percentage ?? 0);
	const cost = (snapshot.cost?.total_cost_usd ?? 0).toFixed(3);
	const modelName = snapshot.model?.display_name || snapshot.model?.id || 'Claude';
	const effortLevel = snapshot.effort?.level;
	const isThinking = snapshot.thinking?.enabled;
	const exceeds200k = snapshot.exceeds_200k_tokens;
	const fiveHourRate = snapshot.rate_limits?.five_hour?.used_percentage;

	return (
		<div className="flex h-7 items-center justify-between border-b border-border/40 bg-zinc-950/90 px-3 text-[11px] text-zinc-300 font-mono select-none">
			{/* Left Section: Model, Effort & Thinking */}
			<div className="flex items-center gap-2 overflow-hidden">
				<div className="flex items-center gap-1 text-zinc-100 font-medium shrink-0">
					<Cpu className="h-3 w-3 text-sky-400" />
					<span className="truncate max-w-[130px]">{modelName}</span>
				</div>

				{effortLevel && (
					<span className="rounded bg-zinc-800 px-1.5 py-0.5 text-[10px] text-zinc-400">
						effort: {effortLevel}
					</span>
				)}

				{isThinking && (
					<span className="flex items-center gap-1 rounded bg-purple-950/60 border border-purple-800/40 px-1.5 py-0.5 text-[10px] text-purple-300">
						<Activity className="h-2.5 w-2.5 text-purple-400 animate-pulse" />
						thinking
					</span>
				)}
			</div>

			{/* Center Section: Context Window Gauge */}
			<div className="flex items-center gap-2 shrink-0">
				<div className="flex items-center gap-1.5">
					<Gauge className="h-3 w-3 text-emerald-400" />
					<span>CTX: {usedPct}%</span>
					<div className="h-1.5 w-16 rounded-full bg-zinc-800 overflow-hidden">
						<div
							className={`h-full transition-all duration-300 ${
								usedPct > 80 ? 'bg-amber-500' : usedPct > 90 ? 'bg-rose-500' : 'bg-emerald-500'
							}`}
							style={{ width: `${Math.min(100, Math.max(0, usedPct))}%` }}
						/>
					</div>
				</div>

				{exceeds200k && (
					<span className="flex items-center gap-1 rounded bg-amber-950/80 border border-amber-800/60 px-1.5 py-0.5 text-[10px] text-amber-300">
						<AlertTriangle className="h-3 w-3 text-amber-400" />
						&gt;200k tokens
					</span>
				)}
			</div>

			{/* Right Section: Cost & Rate Limits */}
			<div className="flex items-center gap-2 shrink-0">
				{fiveHourRate !== undefined && (
					<div
						className="flex items-center gap-1 text-zinc-400 text-[10px]"
						title="5-hour rate limit used"
					>
						<ShieldAlert className="h-2.5 w-2.5 text-zinc-400" />
						<span>5h: {Math.round(fiveHourRate)}%</span>
					</div>
				)}

				<div className="flex items-center gap-0.5 font-semibold text-emerald-400">
					<DollarSign className="h-3 w-3" />
					<span>{cost}</span>
				</div>
			</div>
		</div>
	);
}
