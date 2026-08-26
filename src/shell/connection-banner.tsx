import { useEffect, useState } from 'react';
import { connectionStateStore, type RemoteConnectionInfo } from '@/lib/transport/connection-state';

/**
 * Global connection state banner matching locked design D-03 (connstate-b-marker.html).
 * Displays when remote connection is lost or reconnecting.
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

	const terminalsText = info.activeTerminals === 1 ? '1 terminal' : `${info.activeTerminals || 2} terminals`;
	const agentTurnsText = info.activeAgentTurns === 1 ? '1 agent turn' : `${info.activeAgentTurns || 1} agent turns`;
	const summaryText = `${terminalsText} and ${agentTurnsText} are still running on the host.`;

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
				style={{
					background: 'var(--warning, #eab308)',
				}}
			/>
			<b className="font-semibold" style={{ color: 'var(--warning, #eab308)' }}>
				{info.state === 'reconnecting' ? 'Reconnecting' : 'Disconnected'}
			</b>
			<span className="text-muted-foreground">— {summaryText}</span>
			<span
				className="ml-auto font-mono text-[10px]"
				style={{ color: 'var(--fg-faint, rgba(255, 255, 255, 0.4))' }}
			>
				{info.state === 'reconnecting'
					? `attempt ${info.attempt || 1} · next in ${countdown}s`
					: 'connection dropped'}
			</span>
		</div>
	);
}
