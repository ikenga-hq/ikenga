import { useEffect, useState } from 'react';
import { connectionStateStore, type RemoteConnectionInfo } from '@/lib/transport/connection-state';

function plural(n: number, one: string, many: string): string {
	return `${n} ${n === 1 ? one : many}`;
}

/**
 * Build the "still running on the host" line from what is actually registered.
 *
 * Every number here is counted, never defaulted. An earlier revision fell back
 * to `count || 2` when nothing had registered, so the banner permanently told
 * the user that "2 terminals and 1 agent turns are still running" — a figure
 * taken from the design mockup and presented as live host state.
 */
export function summarize(info: RemoteConnectionInfo): string {
	const parts: string[] = [];
	if (info.activeTerminals > 0) parts.push(plural(info.activeTerminals, 'terminal', 'terminals'));
	if (info.activeAgentTurns > 0)
		parts.push(plural(info.activeAgentTurns, 'agent turn', 'agent turns'));

	if (parts.length === 0) return 'Reconnecting to the host.';
	const subject = parts.join(' and ');
	const verb =
		parts.length === 1 && info.activeTerminals + info.activeAgentTurns === 1 ? 'is' : 'are';
	return `${subject} ${verb} still running on the host.`;
}

/**
 * Global connection state banner matching locked design D-03
 * (connstate-b-marker.html). Displays when the remote connection is lost.
 */
export function ConnectionBanner() {
	const [info, setInfo] = useState<RemoteConnectionInfo>(() => connectionStateStore.get());
	const [countdown, setCountdown] = useState<number>(0);

	useEffect(() => {
		return connectionStateStore.subscribe((nextInfo) => {
			setInfo(nextInfo);
			setCountdown(Math.ceil(nextInfo.nextRetryDelayMs / 1000));
		});
	}, []);

	useEffect(() => {
		if (info.state !== 'reconnecting' || countdown <= 0) return;
		const timer = setInterval(() => {
			setCountdown((prev) => (prev > 1 ? prev - 1 : 0));
		}, 1000);
		return () => clearInterval(timer);
	}, [info.state, countdown]);

	if (info.state === 'connected') return null;

	return (
		<div
			className="flex items-center gap-2.5 px-4 py-2 text-xs border-b transition-colors"
			style={{
				background: 'var(--warning-soft, rgba(234, 179, 8, 0.12))',
				borderColor: 'var(--border-soft, rgba(234, 179, 8, 0.25))',
				fontSize: 'var(--text-body-sm, 13px)',
			}}
			data-testid="connection-banner"
		>
			<span
				className="w-1.5 h-1.5 rounded-full animate-pulse flex-shrink-0"
				style={{ background: 'var(--warning, #eab308)' }}
			/>
			<b className="font-semibold" style={{ color: 'var(--warning, #eab308)' }}>
				{info.state === 'reconnecting' ? 'Reconnecting' : 'Disconnected'}
			</b>
			<span className="text-muted-foreground">— {summarize(info)}</span>
			<span
				className="ml-auto font-mono text-[10px]"
				style={{ color: 'var(--fg-faint, rgba(255, 255, 255, 0.4))' }}
			>
				{info.state === 'reconnecting'
					? `attempt ${info.attempt} · next in ${countdown}s`
					: 'connection dropped'}
			</span>
		</div>
	);
}
