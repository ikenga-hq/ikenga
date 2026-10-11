// One muted line for what this server does not run (see lib/ngwa/server-limits).
//
// The headless daemon cannot supervise pkg sidecars, evaluate trust or place
// engine assets. That is how the server is built, so it reads as a quiet,
// neutral note with a "Details" disclosure — not as one red banner per
// capability. Real failures keep their own error banners elsewhere.

import { Info } from 'lucide-react';
import { serverLimitsLine, type ServerLimit } from '@/lib/ngwa/server-limits';

export function NgwaServerLimits({ limits }: { limits: ReadonlyArray<ServerLimit> }) {
	if (limits.length === 0) return null;
	return (
		<div className="server-limits" role="status" data-server-limits>
			<Info className="h-3.5 w-3.5 flex-none" aria-hidden />
			<span className="server-limits-body">
				<span data-server-limits-line>{serverLimitsLine(limits)}</span>
				<details className="server-limits-details">
					<summary>Details</summary>
					<ul>
						{limits.map((l) => (
							<li key={l.source} data-unavailable={l.source}>
								<strong>{l.label}</strong>
								<span> — {l.reason}</span>
							</li>
						))}
					</ul>
				</details>
			</span>
		</div>
	);
}
