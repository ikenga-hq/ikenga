import { listen } from '@tauri-apps/api/event';
import {
	isPermissionGranted,
	requestPermission,
	sendNotification,
} from '@tauri-apps/plugin-notification';
import { Bell, Check, ShieldAlert, X } from 'lucide-react';
import { useEffect, useState } from 'react';
import { Button } from '@/components/ui/button';
import { iykeFetch } from '@/lib/iyke/client';

export interface PermissionRequestEntry {
	id: string;
	tool_name: string;
	tool_input?: Record<string, unknown>;
	prompt?: string;
	status: 'pending' | 'approved' | 'denied';
	timestamp: number;
}

export function PermissionInbox({ sessionId }: { sessionId: string }) {
	const [requests, setRequests] = useState<PermissionRequestEntry[]>([]);

	useEffect(() => {
		// Initialize desktop notification permissions
		(async () => {
			let granted = await isPermissionGranted();
			if (!granted) {
				const permission = await requestPermission();
				granted = permission === 'granted';
			}
		})();

		let unlisten: (() => void) | undefined;

		listen<{
			ikenga_terminal_id?: string;
			hook_event_name?: string;
			session_id?: string;
			tool_name?: string;
			tool_input?: Record<string, unknown>;
			prompt?: string;
		}>('hooks://event', (event) => {
			const p = event.payload;
			if (!p) return;
			if (sessionId && p.ikenga_terminal_id && p.ikenga_terminal_id !== sessionId) return;

			if (p.hook_event_name === 'PermissionRequest') {
				const newEntry: PermissionRequestEntry = {
					id: `perm-${Date.now()}-${Math.random()}`,
					tool_name: p.tool_name || 'Action',
					tool_input: p.tool_input,
					prompt: p.prompt,
					status: 'pending',
					timestamp: Date.now(),
				};

				setRequests((prev) => [newEntry, ...prev]);

				// Trigger OS toast notification
				sendNotification({
					title: 'Chi Permission Request',
					body: `Approval required for tool ${p.tool_name || 'action'}`,
				});
			} else if (p.hook_event_name === 'Notification' || p.hook_event_name === 'Stop') {
				sendNotification({
					title: 'Ikenga Assistant Update',
					body: p.prompt || 'Assistant finished execution turn',
				});
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

	const handleDecision = (id: string, decision: 'approved' | 'denied') => {
		setRequests((prev) => prev.map((r) => (r.id === id ? { ...r, status: decision } : r)));

		// Post decision back to backend bridge
		// Live endpoint + bearer token. The route itself did not exist until
		// ikenga#149 either, so this POST was a 404 on a port nothing listened on.
		iykeFetch('/iyke/hooks/decision', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ requestId: id, decision }),
		}).catch(() => {});
	};

	if (requests.length === 0) {
		return (
			<div className="flex h-full flex-col items-center justify-center p-6 text-center text-xs text-muted-foreground font-mono select-none">
				<ShieldAlert className="mb-2 h-6 w-6 text-muted-foreground/40" />
				<p className="font-semibold text-zinc-300">Permission Inbox & Notifications</p>
				<p className="mt-1 text-[11px]">No active permission requests or notifications.</p>
			</div>
		);
	}

	return (
		<div className="flex h-full flex-col bg-zinc-950 p-3 text-xs font-mono text-zinc-200 select-none overflow-y-auto space-y-2">
			<div className="flex items-center gap-1.5 font-semibold text-amber-400 border-b border-border/40 pb-2">
				<Bell className="h-3.5 w-3.5" />
				<span>
					Permission Inbox ({requests.filter((r) => r.status === 'pending').length} pending)
				</span>
			</div>

			{requests.map((req) => (
				<div
					key={req.id}
					className={`rounded border p-2.5 ${
						req.status === 'pending'
							? 'bg-amber-950/20 border-amber-800/50'
							: req.status === 'approved'
								? 'bg-emerald-950/20 border-emerald-800/40 opacity-70'
								: 'bg-rose-950/20 border-rose-800/40 opacity-70'
					}`}
				>
					<div className="flex items-center justify-between">
						<span className="font-semibold text-zinc-100">{req.tool_name}</span>
						<span className="text-[10px] text-zinc-500">
							{new Date(req.timestamp).toLocaleTimeString()}
						</span>
					</div>

					{req.prompt && <p className="mt-1 text-zinc-300">{req.prompt}</p>}

					{req.tool_input && (
						<pre className="mt-1.5 max-h-24 overflow-x-auto rounded bg-zinc-900/90 p-2 text-[10px] text-zinc-400">
							{JSON.stringify(req.tool_input, null, 2)}
						</pre>
					)}

					{req.status === 'pending' ? (
						<div className="mt-2 flex items-center justify-end gap-2">
							<Button
								size="sm"
								className="h-6 px-2 text-[10px] bg-rose-600 hover:bg-rose-500 text-white"
								onClick={() => handleDecision(req.id, 'denied')}
							>
								<X className="mr-1 h-3 w-3" /> Deny
							</Button>

							<Button
								size="sm"
								className="h-6 px-2 text-[10px] bg-emerald-600 hover:bg-emerald-500 text-white"
								onClick={() => handleDecision(req.id, 'approved')}
							>
								<Check className="mr-1 h-3 w-3" /> Approve
							</Button>
						</div>
					) : (
						<div className="mt-1.5 text-[10px] font-semibold uppercase tracking-wider">
							Status:{' '}
							<span className={req.status === 'approved' ? 'text-emerald-400' : 'text-rose-400'}>
								{req.status}
							</span>
						</div>
					)}
				</div>
			))}
		</div>
	);
}
