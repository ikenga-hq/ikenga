// The status bar's updater progress segment
// (`06-interaction-spec.md` §3.13 #90: "Update download progress replaces
// the engine segment while active"; `designs/system-flows.html?state=
// update-flow` step 2: "The progress also runs in the status bar, so you can
// keep working in a pane while it finishes").
//
// Reads the shared `updater-store.ts` (the same store <UpdaterBanner> and
// <UpdateSheet> read/drive) — this is a separate component tree from either
// of those, so it can't reach into their local state; the store is what
// makes the three surfaces agree on one live download.
//
// Mounted in `status-bar.tsx` in place of the plain `engine` read-only
// segment; renders that same segment (unchanged markup) whenever no download
// is active, so idle behaviour is exactly what it was before this WP.

import { Bot, Download } from 'lucide-react';
import { cn } from '@/components/ui/utils';
import { progressPct, useUpdaterStore } from '@/lib/updater/updater-store';
import { ITEM } from '@/shell/status-bar';

export function UpdaterStatusBarProgress({ engine }: { engine: string | null }) {
	const installing = useUpdaterStore((s) => s.installing);
	const version = useUpdaterStore((s) => s.available?.version ?? null);
	const bytesDownloaded = useUpdaterStore((s) => s.bytesDownloaded);
	const totalBytes = useUpdaterStore((s) => s.totalBytes);

	if (installing) {
		const pct = progressPct(bytesDownloaded, totalBytes);
		// The version + percentage are visible text in the segment itself
		// (matching the locked design's always-on-screen "Downloading 0.9.1 ·
		// 28%" label) — `title` stays as a supplement for the full string, not
		// the only place it appears (WP-41-F0).
		const label = `Downloading${version ? ` ${version}` : ''}${pct !== null ? ` · ${pct}%` : '…'}`;
		return (
			<span
				data-seg="updater-progress"
				title={`Downloading Ikenga ${version ?? ''}${pct !== null ? ` — ${pct}%` : ''}`.trim()}
				className={cn(ITEM, 'cursor-default')}
			>
				<Download aria-hidden className="h-3 w-3 shrink-0" />
				<span className="whitespace-nowrap font-mono">{label}</span>
			</span>
		);
	}

	if (!engine) return null;
	return (
		<span
			data-seg="engine"
			title="Engine for the next dispatch — choose it in the Companion"
			className={cn(ITEM, 'cursor-default')}
		>
			<Bot aria-hidden className="h-3 w-3" />
			<span className="font-mono">{engine}</span>
		</span>
	);
}
