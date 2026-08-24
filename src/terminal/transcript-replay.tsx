import { listen } from '@tauri-apps/api/event';
import { ChevronDown, ChevronRight, Code, MessageSquare, Terminal, Wrench } from 'lucide-react';
import { useEffect, useState } from 'react';

export interface MessageContentItem {
	type: 'text' | 'tool_use' | 'thinking';
	text?: string;
	id?: string;
	name?: string;
	input?: Record<string, unknown>;
	thinking?: string;
}

export interface UserTurnRecord {
	type: 'user';
	uuid?: string;
	sessionId?: string;
	timestamp?: string;
	message?: {
		role?: string;
		content?: MessageContentItem[];
	};
}

export interface AssistantTurnRecord {
	type: 'assistant';
	uuid?: string;
	sessionId?: string;
	timestamp?: string;
	message?: {
		id?: string;
		model?: string;
		role?: string;
		content?: MessageContentItem[];
		usage?: {
			input_tokens?: number;
			output_tokens?: number;
		};
	};
}

export interface ToolResultRecord {
	type: 'tool_result';
	uuid?: string;
	sessionId?: string;
	toolUseID?: string;
	toolName?: string;
	content?: unknown;
	success?: boolean;
	error?: string;
}

export type TranscriptEvent = UserTurnRecord | AssistantTurnRecord | ToolResultRecord | { type: string; [key: string]: unknown };

interface TranscriptReplayProps {
	sessionId: string;
}

export function TranscriptReplay({ sessionId }: TranscriptReplayProps) {
	const [records, setRecords] = useState<TranscriptEvent[]>([]);
	const [expandedItems, setExpandedItems] = useState<Record<string, boolean>>({});

	useEffect(() => {
		let unlisten: (() => void) | undefined;

		listen<TranscriptEvent>(`transcript://${sessionId}`, (event) => {
			if (event.payload) {
				setRecords((prev) => [...prev, event.payload]);
			}
		})
			.then((fn) => {
				unlisten = fn;
			})
			.catch(() => {});

		return () => {
			if (unlisten) unlisten();
		};
	}, [sessionId]);

	const toggleExpand = (id: string) => {
		setExpandedItems((prev) => ({ ...prev, [id]: !prev[id] }));
	};

	if (records.length === 0) {
		return (
			<div className="flex h-full flex-col items-center justify-center p-6 text-center text-xs text-muted-foreground font-mono">
				<MessageSquare className="mb-2 h-6 w-6 text-muted-foreground/40" />
				<p className="font-semibold text-zinc-300">Transcript Replay Pane</p>
				<p className="mt-1 text-[11px]">Streaming session {sessionId.slice(0, 8)} turns live...</p>
			</div>
		);
	}

	return (
		<div className="flex h-full flex-col overflow-y-auto bg-zinc-950 p-3 text-xs font-mono text-zinc-200 divide-y divide-zinc-800/40">
			{records.map((rec, idx) => {
				const key = (rec as { uuid?: string }).uuid || `turn-${idx}`;
				const isExpanded = expandedItems[key] ?? true;

				if (rec.type === 'user') {
					const userRec = rec as UserTurnRecord;
					const text = userRec.message?.content?.find((c) => c.type === 'text')?.text || 'User prompt';

					return (
						<div key={key} className="py-2">
							<div
								onClick={() => toggleExpand(key)}
								className="flex items-center gap-2 cursor-pointer text-sky-400 font-semibold hover:text-sky-300"
							>
								{isExpanded ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
								<Terminal className="h-3 w-3" />
								<span>USER</span>
								{userRec.timestamp && (
									<span className="text-[10px] text-zinc-500 font-normal">
										{new Date(userRec.timestamp).toLocaleTimeString()}
									</span>
								)}
							</div>

							{isExpanded && (
								<div className="mt-1.5 rounded bg-sky-950/20 border border-sky-900/30 p-2.5 text-zinc-300 whitespace-pre-wrap leading-relaxed">
									{text}
								</div>
							)}
						</div>
					);
				}

				if (rec.type === 'assistant') {
					const asstRec = rec as AssistantTurnRecord;
					const contentItems = asstRec.message?.content || [];

					return (
						<div key={key} className="py-2">
							<div
								onClick={() => toggleExpand(key)}
								className="flex items-center gap-2 cursor-pointer text-purple-400 font-semibold hover:text-purple-300"
							>
								{isExpanded ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
								<Code className="h-3 w-3" />
								<span>ASSISTANT</span>
								{asstRec.message?.model && (
									<span className="rounded bg-zinc-800 px-1 text-[10px] text-zinc-400 font-normal">
										{asstRec.message.model}
									</span>
								)}
							</div>

							{isExpanded && (
								<div className="mt-1.5 flex flex-col gap-2">
									{contentItems.map((item, itemIdx) => {
										if (item.type === 'text') {
											return (
												<div
													key={itemIdx}
													className="rounded bg-zinc-900/60 border border-zinc-800/50 p-2.5 leading-relaxed text-zinc-200 whitespace-pre-wrap"
												>
													{item.text}
												</div>
											);
										}

										if (item.type === 'tool_use') {
											return (
												<div
													key={itemIdx}
													className="rounded bg-amber-950/20 border border-amber-900/40 p-2 text-amber-200"
												>
													<div className="flex items-center gap-1.5 font-semibold text-amber-400 text-[11px]">
														<Wrench className="h-3 w-3" />
														<span>Tool: {item.name}</span>
													</div>
													{item.input && (
														<pre className="mt-1 max-h-32 overflow-x-auto rounded bg-zinc-950 p-2 text-[10px] text-zinc-400">
															{JSON.stringify(item.input, null, 2)}
														</pre>
													)}
												</div>
											);
										}

										return null;
									})}
								</div>
							)}
						</div>
					);
				}

				if (rec.type === 'tool_result') {
					const toolRes = rec as ToolResultRecord;
					return (
						<div key={key} className="py-2 text-[11px]">
							<div className="flex items-center gap-1.5 text-emerald-400 font-medium">
								<Wrench className="h-3 w-3" />
								<span>Tool Result [{toolRes.toolName || 'tool'}]</span>
								<span className={`text-[10px] ${toolRes.success ? 'text-emerald-400' : 'text-rose-400'}`}>
									{toolRes.success ? '✓ success' : '✗ failed'}
								</span>
							</div>
							{toolRes.error && <p className="mt-1 text-rose-400">{toolRes.error}</p>}
						</div>
					);
				}

				return null;
			})}
		</div>
	);
}
