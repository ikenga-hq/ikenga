import { Bell, Check, ShieldAlert, X } from 'lucide-react';
import { useEffect, useState } from 'react';
import { Button } from '@/components/ui/button';
import { iykeFetch } from '@/lib/iyke/client';
import { listen, settingsGet, settingsSet } from '@/lib/tauri-cmd';
import {
	isNotificationPermissionGranted,
	requestNotificationPermission,
	sendDesktopNotification,
} from '@/lib/transport/shims';

export interface PermissionRequestEntry {
	id: string;
	request_id: string;
	event_type: 'permission' | 'tool_use';
	tool_name: string;
	tool_input?: Record<string, unknown>;
	prompt?: string;
	status: 'pending' | 'approved' | 'denied';
	timestamp: number;
}

function holdSettingKey(sessionId: string) {
	return `permissions.hold_terminal_${sessionId}`;
}

export function PermissionInbox({ sessionId }: { sessionId: string }) {
	const [requests, setRequests] = useState<PermissionRequestEntry[]>([]);
	const [holdEnabled, setHoldEnabled] = useState(false);

	useEffect(() => {
		// Initialize desktop notification permissions
		(async () => {
			let granted = await isNotificationPermissionGranted();
			if (!granted) {
				const permission = await requestNotificationPermission();
				granted = permission === 'granted';
			}
		})();

		// Read whether this terminal has PreToolUse gating enabled.
		settingsGet(holdSettingKey(sessionId))
			.then((v) => setHoldEnabled(v === 'true' || v === '1'))
			.catch(() => {});

		let unlisten: (() => void) | undefined;

		listen<{
			request_id?: string;
			ikenga_terminal_id?: string;
			hook_event_name?: string;
			session_id?: string;
			tool_name?: string;
			tool_input?: Record<string, unknown>;
			prompt?: string;
			held?: boolean;
		}>('hooks://event', (event) => {
			const p = event.payload;
			if (!p) return;
			if (sessionId && p.ikenga_terminal_id && p.ikenga_terminal_id !== sessionId) return;

			if (p.hook_event_name === 'PermissionRequest') {
				const newEntry: PermissionRequestEntry = {
					id: p.request_id || `perm-${Date.now()}-${Math.random()}`,
					request_id: p.request_id || `perm-${Date.now()}-${Math.random()}`,
					event_type: 'permission',
					tool_name: p.tool_name || 'Action',
					tool_input: p.tool_input,
					prompt: p.prompt,
					status: 'pending',
					timestamp: Date.now(),
				};

				setRequests((prev) => [newEntry, ...prev]);

				// Trigger OS toast notification
				sendDesktopNotification({
					title: 'Chi Permission Request',
					body: `Approval required for tool ${p.tool_name || 'action'}`,
				});
			} else if (p.hook_event_name === 'PreToolUse' && p.held && p.request_id) {
				const newEntry: PermissionRequestEntry = {
					id: p.request_id,
					request_id: p.request_id,
					event_type: 'tool_use',
					tool_name: p.tool_name || 'Tool use',
					tool_input: p.tool_input,
					prompt: p.prompt,
					status: 'pending',
					timestamp: Date.now(),
				};

				setRequests((prev) => [newEntry, ...prev]);

				sendDesktopNotification({
					title: 'Tool Use Request',
					body: `Claude wants to use ${p.tool_name || 'a tool'} — approve?`,
				});
			} else if (p.hook_event_name === 'Notification' || p.hook_event_name === 'Stop') {
				sendDesktopNotification({
					title: 'Ikenga Assistant Update',
					body: p.prompt || 'Assistant finished execution turn',
				});
			}
		})
			.then((fn) => {
				unlisten = fn;
			})
			.catch(() => {});

		// Listen for decisions so we can mark held requests as resolved even if
		// the response came from another surface (or a timeout).
		let unlistenDecision: (() => void) | undefined;
		listen<HookDecisionBody>('hooks://decision', (event) => {
			const d = event.payload;
			if (!d?.requestId) return;
			setRequests((prev) =>
				prev.map((r) =>
					r.id === d.requestId
						? { ...r, status: d.decision === 'approved' ? 'approved' : 'denied' }
						: r
				)
			);
		})
			.then((fn) => {
				unlistenDecision = fn;
			})
			.catch(() => {});

		return () => {
			if (unlisten) unlisten();
			if (unlistenDecision) unlistenDecision();
		};
	}, [sessionId]);

	const handleDecision = (id: string, decision: 'approved' | 'denied') => {
		setRequests((prev) => prev.map((r) => (r.id === id ? { ...r, status: decision } : r)));

		const req = requests.find((r) => r.id === id);
		const requestId = req?.request_id || id;

		// Post decision back to backend bridge
		iykeFetch('/iyke/hooks/decision', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ requestId, decision }),
		}).catch(() => {});
	};

	async function toggleHold() {
		const next = !holdEnabled;
		setHoldEnabled(next);
		try {
			await settingsSet(holdSettingKey(sessionId), next ? 'true' : 'false');
		} catch {
			setHoldEnabled((v) => !v);
		}
	}

	return (
		<div className="flex h-full flex-col bg-zinc-950 p-3 text-xs font-mono text-zinc-200 select-none overflow-y-auto space-y-2">
			<div className="flex items-center justify-between gap-2 border-b border-border/40 pb-2">
				<div className="flex items-center gap-1.5 font-semibold text-amber-400">
					<Bell className="h-3.5 w-3.5" />
					<span>
						Permission Inbox ({requests.filter((r) => r.status === 'pending').length} pending)
					</span>
				</div>
				<label className="flex items-center gap-1.5 text-[10px] text-zinc-400">
					<input
						type="checkbox"
						checked={holdEnabled}
						onChange={() => void toggleHold()}
						className="h-3 w-3 rounded border-border bg-background"
					/>
					Hold PreToolUse
				</label>
			</div>

			{requests.length === 0 ? (
				<div className="flex h-full flex-col items-center justify-center p-6 text-center text-xs text-muted-foreground font-mono select-none">
					<ShieldAlert className="mb-2 h-6 w-6 text-muted-foreground/40" />
					<p className="font-semibold text-zinc-300">Permission Inbox & Notifications</p>
					<p className="mt-1 text-[11px]">No active permission requests or notifications.</p>
				</div>
			) : (
				requests.map((req) => (
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
							<span className="font-semibold text-zinc-100">
								{req.event_type === 'tool_use' ? `Tool use: ${req.tool_name}` : req.tool_name}
							</span>
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
				))
			)}
		</div>
	);
}

type HookDecisionBody = { requestId?: string; decision?: string };
