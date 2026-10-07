import { Bell, Check, ShieldAlert, X } from 'lucide-react';
import { useEffect, useState } from 'react';
import { Button } from '@/components/ui/button';
import { settingsGet, settingsSet } from '@/lib/tauri-cmd';
import {
	browserNotificationPermission,
	isBrowserHost,
	isNotificationPermissionGranted,
	listen,
	requestNotificationPermission,
	sendNotification,
} from '@/lib/transport';
import {
	decideHookRequest,
	hostDecideBlock,
	refreshHostDecideBlock,
} from '@/shell/notifications/actions';

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
	// G-ACCESS §5.1 / §5.7 (WP-75): when asks are routed to another device,
	// this inbox renders them read-only with the reason.
	const [blocked, setBlocked] = useState<string | null>(hostDecideBlock());
	const [error, setError] = useState<string | null>(null);
	// Browser only: whether the explicit "Enable notifications" control shows.
	const [canEnableNotifications, setCanEnableNotifications] = useState(
		() => isBrowserHost() && browserNotificationPermission() === 'default'
	);

	async function enableNotifications() {
		await requestNotificationPermission();
		setCanEnableNotifications(browserNotificationPermission() === 'default');
	}

	useEffect(() => {
		let live = true;
		void refreshHostDecideBlock().then((b) => {
			if (live) setBlocked(b);
		});
		return () => {
			live = false;
		};
	}, []);

	useEffect(() => {
		// Initialize desktop notification permissions. A browser only honours
		// the prompt from a click, so there it is the "Enable notifications"
		// button below, not an on-mount request.
		if (!isBrowserHost()) {
			(async () => {
				let granted = await isNotificationPermissionGranted();
				if (!granted) {
					const permission = await requestNotificationPermission();
					granted = permission === 'granted';
				}
			})();
		}

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

				// No OS toast here: WP-40 records this ask as a `permission`
				// notification row, and the notification centre's toast is its copy.
				setRequests((prev) => [newEntry, ...prev]);
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

				// Held gate: also a WP-40 `permission` row — no second OS toast.
				setRequests((prev) => [newEntry, ...prev]);
			} else if (p.hook_event_name === 'Notification') {
				// Not Stop: WP-40 now registers it, and a raw OS toast every turn end would bypass the notification centre's mute.
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

	const handleDecision = async (id: string, decision: 'approved' | 'denied') => {
		const req = requests.find((r) => r.id === id);
		const requestId = req?.request_id || id;
		setError(null);

		// G-ACCESS §5.5: the held gate's row goes through `permission_decide`
		// (routing-capped, attributed, audited); the checked hooks route is the
		// fallback while the row is not recorded yet. A refusal is shown,
		// never presented as an answer (review WP75-R10).
		const refused = await decideHookRequest(requestId, decision);
		if (refused) {
			setError(refused);
			setBlocked(hostDecideBlock());
			return;
		}
		setRequests((prev) => prev.map((r) => (r.id === id ? { ...r, status: decision } : r)));
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
		<div className="flex h-full flex-col bg-card p-3 text-xs font-mono text-foreground select-none overflow-y-auto space-y-2">
			<div className="flex items-center justify-between gap-2 border-b border-border/40 pb-2">
				<div className="flex items-center gap-1.5 font-semibold text-amber-400">
					<Bell className="h-3.5 w-3.5" />
					<span>
						Permission Inbox ({requests.filter((r) => r.status === 'pending').length} pending)
					</span>
				</div>
				<label className="flex items-center gap-1.5 text-[10px] text-muted-foreground">
					<input
						type="checkbox"
						checked={holdEnabled}
						onChange={() => void toggleHold()}
						className="h-3 w-3 rounded border-border bg-background"
					/>
					Hold PreToolUse
				</label>
			</div>

			{canEnableNotifications && (
				<div className="flex items-center justify-between gap-2 text-[10px] text-muted-foreground">
					<span>Get a browser notification when Claude needs you.</span>
					<Button
						size="sm"
						variant="outline"
						className="h-6 px-2 text-[10px]"
						onClick={() => void enableNotifications()}
					>
						Enable notifications
					</Button>
				</div>
			)}

			{error && (
				<p role="alert" className="text-[10px] text-rose-400">
					{error}
				</p>
			)}

			{requests.length === 0 ? (
				<div className="flex h-full flex-col items-center justify-center p-6 text-center text-xs text-muted-foreground font-mono select-none">
					<ShieldAlert className="mb-2 h-6 w-6 text-muted-foreground/40" />
					<p className="font-semibold text-foreground">Permission Inbox & Notifications</p>
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
							<span className="font-semibold text-foreground">
								{req.event_type === 'tool_use' ? `Tool use: ${req.tool_name}` : req.tool_name}
							</span>
							<span className="text-[10px] text-muted-foreground">
								{new Date(req.timestamp).toLocaleTimeString()}
							</span>
						</div>

						{req.prompt && <p className="mt-1 text-foreground">{req.prompt}</p>}

						{req.tool_input && (
							<pre className="mt-1.5 max-h-24 overflow-x-auto rounded bg-muted p-2 text-[10px] text-muted-foreground">
								{JSON.stringify(req.tool_input, null, 2)}
							</pre>
						)}

						{req.status === 'pending' && blocked ? (
							<p data-waiting-on="device" className="mt-2 text-[10px] text-muted-foreground">
								{blocked}
							</p>
						) : req.status === 'pending' ? (
							<div className="mt-2 flex items-center justify-end gap-2">
								<Button
									size="sm"
									className="h-6 px-2 text-[10px] bg-rose-600 hover:bg-rose-500 text-white"
									onClick={() => void handleDecision(req.id, 'denied')}
								>
									<X className="mr-1 h-3 w-3" /> Deny
								</Button>

								<Button
									size="sm"
									className="h-6 px-2 text-[10px] bg-emerald-600 hover:bg-emerald-500 text-white"
									onClick={() => void handleDecision(req.id, 'approved')}
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
