import { listen } from '@tauri-apps/api/event';
import {
	Activity,
	CheckCircle2,
	Clock,
	Code,
	FileText,
	Globe,
	Search,
	Terminal,
	XCircle,
} from 'lucide-react';
import { useEffect, useState } from 'react';
import { iykeFetch } from '@/lib/iyke/client';

export interface HookEventPayload {
	hook_event_name?: string;
	session_id?: string;
	transcript_path?: string;
	cwd?: string;
	permission_mode?: string;
	tool_name?: string;
	tool_input?: Record<string, unknown>;
	tool_output?: Record<string, unknown>;
	tool_use_id?: string;
	prompt?: string;
	sessionTitle?: string;
	stopReason?: string;
}

export interface ToolCallEntry {
	tool_use_id: string;
	tool_name: string;
	tool_input?: Record<string, unknown>;
	tool_output?: Record<string, unknown>;
	status: 'running' | 'done' | 'failed';
	startTime: number;
	endTime?: number;
	durationMs?: number;
}

export function ToolCallFeed({ sessionId }: { sessionId: string }) {
	const [calls, setCalls] = useState<ToolCallEntry[]>([]);

	useEffect(() => {
		// Initial fetch from backend REST endpoint if available
		// Live endpoint + bearer token. This used to be a hardcoded
		// `http://127.0.0.1:4000`, which the bridge has never bound — it takes a
		// dynamic port. So this feed had two independent reasons to stay empty
		// (ikenga#149): nothing was posting events, and nothing could read them.
		iykeFetch('/iyke/hooks/events')
			.then((res) => (res.ok ? res.json() : []))
			.then((events: HookEventPayload[]) => {
				if (Array.isArray(events)) {
					const map = new Map<string, ToolCallEntry>();
					for (const ev of events) {
						if (sessionId && ev.session_id && ev.session_id !== sessionId) continue;
						const id = ev.tool_use_id || `ev-${Math.random()}`;
						if (ev.hook_event_name === 'PreToolUse') {
							map.set(id, {
								tool_use_id: id,
								tool_name: ev.tool_name || 'Tool',
								tool_input: ev.tool_input,
								status: 'running',
								startTime: Date.now(),
							});
						} else if (ev.hook_event_name === 'PostToolUse') {
							const existing = map.get(id);
							if (existing) {
								existing.status = 'done';
								existing.endTime = Date.now();
								existing.durationMs = existing.endTime - existing.startTime;
								existing.tool_output = ev.tool_output;
							} else {
								map.set(id, {
									tool_use_id: id,
									tool_name: ev.tool_name || 'Tool',
									tool_input: ev.tool_input,
									tool_output: ev.tool_output,
									status: 'done',
									startTime: Date.now(),
								});
							}
						}
					}
					setCalls(Array.from(map.values()));
				}
			})
			.catch(() => {});

		let unlisten: (() => void) | undefined;
		listen<HookEventPayload>('hooks://event', (event) => {
			const payload = event.payload;
			if (!payload) return;
			if (sessionId && payload.session_id && payload.session_id !== sessionId) return;

			const id = payload.tool_use_id || `tool-${Date.now()}-${Math.random()}`;

			setCalls((prev) => {
				const existingIndex = prev.findIndex((c) => c.tool_use_id === id);

				if (payload.hook_event_name === 'PreToolUse') {
					if (existingIndex >= 0) return prev;
					return [
						...prev,
						{
							tool_use_id: id,
							tool_name: payload.tool_name || 'Tool',
							tool_input: payload.tool_input,
							status: 'running',
							startTime: Date.now(),
						},
					];
				}

				if (payload.hook_event_name === 'PostToolUse') {
					if (existingIndex >= 0) {
						const updated = [...prev];
						const item = { ...updated[existingIndex] };
						item.status = 'done';
						item.endTime = Date.now();
						item.durationMs = item.endTime - item.startTime;
						item.tool_output = payload.tool_output;
						updated[existingIndex] = item;
						return updated;
					}
					return [
						...prev,
						{
							tool_use_id: id,
							tool_name: payload.tool_name || 'Tool',
							tool_input: payload.tool_input,
							tool_output: payload.tool_output,
							status: 'done',
							startTime: Date.now(),
						},
					];
				}

				return prev;
			});
		})
			.then((fn) => {
				unlisten = fn;
			})
			.catch(() => {});

		return () => {
			if (unlisten) unlisten();
		};
	}, [sessionId]);

	const getToolIcon = (name: string) => {
		const lower = name.toLowerCase();
		if (lower.includes('bash') || lower.includes('command'))
			return <Terminal className="h-3.5 w-3.5 text-emerald-400" />;
		if (lower.includes('edit') || lower.includes('write'))
			return <Code className="h-3.5 w-3.5 text-sky-400" />;
		if (lower.includes('read') || lower.includes('file'))
			return <FileText className="h-3.5 w-3.5 text-amber-400" />;
		if (lower.includes('search') || lower.includes('grep'))
			return <Search className="h-3.5 w-3.5 text-purple-400" />;
		if (lower.includes('fetch') || lower.includes('web'))
			return <Globe className="h-3.5 w-3.5 text-blue-400" />;
		return <Activity className="h-3.5 w-3.5 text-zinc-400" />;
	};

	if (calls.length === 0) {
		return (
			<div className="flex h-full flex-col items-center justify-center p-6 text-center text-xs text-muted-foreground font-mono select-none">
				<Clock className="mb-2 h-6 w-6 text-muted-foreground/40" />
				<p className="font-semibold text-zinc-300">Live Tool-Call Feed</p>
				<p className="mt-1 text-[11px]">Listening for PreToolUse and PostToolUse events...</p>
			</div>
		);
	}

	return (
		<div className="flex h-full flex-col overflow-y-auto bg-zinc-950 p-3 text-xs font-mono text-zinc-200 divide-y divide-zinc-800/40 select-none">
			{calls.map((call) => (
				<div key={call.tool_use_id} className="py-2.5">
					<div className="flex items-center justify-between">
						<div className="flex items-center gap-2">
							{getToolIcon(call.tool_name)}
							<span className="font-semibold text-zinc-100">{call.tool_name}</span>
							{call.status === 'running' ? (
								<span className="flex items-center gap-1 text-[10px] text-amber-400">
									<Activity className="h-2.5 w-2.5 animate-spin" /> running
								</span>
							) : call.status === 'done' ? (
								<span className="flex items-center gap-1 text-[10px] text-emerald-400">
									<CheckCircle2 className="h-2.5 w-2.5" /> done
								</span>
							) : (
								<span className="flex items-center gap-1 text-[10px] text-rose-400">
									<XCircle className="h-2.5 w-2.5" /> failed
								</span>
							)}
						</div>

						{call.durationMs !== undefined && (
							<span className="text-[10px] text-zinc-500">{call.durationMs}ms</span>
						)}
					</div>

					{call.tool_input && (
						<pre className="mt-1.5 max-h-28 overflow-x-auto rounded bg-zinc-900/80 border border-zinc-800/60 p-2 text-[10px] text-zinc-400">
							{JSON.stringify(call.tool_input, null, 2)}
						</pre>
					)}
				</div>
			))}
		</div>
	);
}
