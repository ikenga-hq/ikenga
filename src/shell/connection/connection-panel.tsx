// The "Connection" panel on the Ngwa Health page: the viewer's own round trip
// to the server, as "340 ms ± 85", coloured by what it means for typing.
// Every browser viewer sees it (it is their link, not the server's state);
// the desktop does not (its "server" is this process).
//
// Measuring is `useConnectionRtt()` (lib/connection): a tiny ping/pong over
// the events socket every 5 s, paused while the tab is hidden.

import { Wifi } from 'lucide-react';

import { type RttLevel, type RttSummary, explainRtt, formatRtt } from '@/lib/connection/rtt';
import { mayMeasureConnection, useConnectionRtt } from '@/lib/connection/rtt-monitor';
import { mayAskForServerHealth } from '@/lib/queries/server-health';

export const LEVEL_WORD: Record<RttLevel, string> = {
	good: 'Good',
	warn: 'Slow',
	bad: 'Poor',
	unknown: 'Measuring',
};

/** `ok` / `bad` are the Health page's existing count classes; `warn` is ours. */
const LEVEL_CLASS: Record<RttLevel, string> = {
	good: 'ok',
	warn: 'warn',
	bad: 'bad',
	unknown: '',
};

/** Panel body, pure over a summary (tested without a socket). */
export function ConnectionPanelBody({
	summary,
	onOpenServer,
}: {
	summary: RttSummary;
	/** Present for an admin: opens the Server card. */
	onOpenServer?: () => void;
}) {
	return (
		<>
			<h3>
				<Wifi className="h-3.5 w-3.5" />
				<span>Connection</span>
				<span
					className={`n ${LEVEL_CLASS[summary.level]}`}
					data-conn-level={summary.level}
					data-testid="connection-level"
				>
					{LEVEL_WORD[summary.level]}
				</span>
			</h3>
			<div className="hlist">
				<div className="hrow" data-conn="rtt">
					<div className="txt">
						<span className="t1" data-testid="connection-figure">
							{formatRtt(summary)}
						</span>
						<span className="t2" data-testid="connection-explain">
							{explainRtt(summary)}
						</span>
						<span className="t2">
							Round trip from this browser to the server, median and jitter over the last{' '}
							{summary.count} {summary.count === 1 ? 'check' : 'checks'}
							{summary.via === 'http' ? ' (timed request)' : ''}. Paused while this tab is hidden.
						</span>
					</div>
				</div>
				{onOpenServer && (
					<div className="hrow" data-conn="server-link">
						<div className="txt">
							<span className="t1">Server health</span>
							<span className="t2">Memory, disk, backups and tunnels on the server.</span>
						</div>
						<div className="acts">
							<button type="button" className="chip on" onClick={onOpenServer}>
								Open
							</button>
						</div>
					</div>
				)}
			</div>
		</>
	);
}

/** Wired panel. Renders nothing off the browser. */
export function ConnectionPanel({ onOpenServer }: { onOpenServer?: () => void }) {
	const summary = useConnectionRtt();
	if (!mayMeasureConnection()) return null;
	return (
		<ConnectionPanelBody
			summary={summary}
			onOpenServer={onOpenServer && mayAskForServerHealth() ? onOpenServer : undefined}
		/>
	);
}
