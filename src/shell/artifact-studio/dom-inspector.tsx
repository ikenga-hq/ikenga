// Artifact Studio loupe — DOM inspector (right rail "DOM" tab). Moved out of
// `density/loupe.tsx` so its browser-session gate can be tested on its own.

import { RefreshCw, TreePine } from 'lucide-react';
import { type ReactNode, useCallback, useEffect, useState } from 'react';
import { cn } from '@/components/ui/utils';
import {
	fsListenWatch,
	fsUnwatch,
	fsWatch,
	type IykeDomResult,
	iykeDomQuery,
	isRemoteWebSession,
} from '@/lib/tauri-cmd';

/**
 * The right rail's `dom` slot, or `undefined` (which hides the tab) in a
 * browser session (gap audit rank 28). `iyke_dom_query` asks the desktop
 * app's iyke viewer bridge for the iframe's accessibility tree; the headless
 * daemon does not serve it, so the tab only ever showed a raw
 * "DOM probe failed" error there.
 */
export function domInspectorSlot(paneId: string, path: string): ReactNode | undefined {
	if (isRemoteWebSession()) return undefined;
	return <DomInspector paneId={paneId} path={path} />;
}

// ─── DOM inspector ────────────────────────────────────────────────────
//
// Per the unified plan's Open Question 7 (locked in Phase 2): the iyke
// viewer-server injects an iframe bridge into every served artifact;
// the bridge responds to `iyke://dom-request` by serializing the
// document's accessibility tree. The `iyke_dom_query` Tauri command
// (commands/iyke.rs) wraps that RPC for in-shell consumers — this
// component is the first one. Auto-refreshes on fs_watch of the
// artifact's parent dir so post-save renders pick up the new tree.

export function DomInspector({ paneId, path }: { paneId: string; path: string }) {
	const [result, setResult] = useState<IykeDomResult | null>(null);
	const [error, setError] = useState<string | null>(null);
	const [loading, setLoading] = useState(false);
	const [filter, setFilter] = useState('');

	const refresh = useCallback(
		async (q?: string) => {
			setLoading(true);
			setError(null);
			try {
				const out = await iykeDomQuery({
					pane: paneId,
					query: q && q.length > 0 ? q : undefined,
				});
				setResult(out);
			} catch (e) {
				setError(e instanceof Error ? e.message : String(e));
			} finally {
				setLoading(false);
			}
		},
		[paneId]
	);

	useEffect(() => {
		void refresh();
	}, [refresh]);

	// Auto-refresh on file save. Watch the artifact's parent dir (same
	// pattern HtmlFrame uses for iframe hot-reload) and debounce so a
	// burst of Create+Modify events only fires one DOM probe.
	useEffect(() => {
		let cancelled = false;
		let watcherId: string | null = null;
		let unlisten: (() => void) | null = null;
		let debounceTimer: ReturnType<typeof setTimeout> | null = null;

		void (async () => {
			try {
				const slash = path.lastIndexOf('/');
				const parent = slash > 0 ? path.slice(0, slash) : path;
				const id = await fsWatch(parent);
				if (cancelled) {
					void fsUnwatch(id);
					return;
				}
				watcherId = id;
				unlisten = await fsListenWatch(id, () => {
					if (cancelled) return;
					if (debounceTimer) clearTimeout(debounceTimer);
					// 250ms past the iframe-reload debounce (100ms) so the new
					// tree reflects the rendered output, not the pre-reload one.
					debounceTimer = setTimeout(() => {
						void refresh(filter);
					}, 250);
				});
			} catch {
				// Watcher best-effort; the manual refresh button still works.
			}
		})();

		return () => {
			cancelled = true;
			if (debounceTimer) clearTimeout(debounceTimer);
			if (unlisten) unlisten();
			if (watcherId) void fsUnwatch(watcherId);
		};
	}, [path, refresh, filter]);

	const onFilterChange = useCallback(
		(next: string) => {
			setFilter(next);
			void refresh(next);
		},
		[refresh]
	);

	return (
		<div className="flex h-full w-full flex-col">
			<div className="flex shrink-0 items-center gap-1.5 border-b border-border bg-muted/10 px-2 py-1.5">
				<TreePine className="h-3 w-3 text-muted-foreground" />
				<input
					type="text"
					value={filter}
					onChange={(e) => onFilterChange(e.target.value)}
					placeholder="filter (role / name / value)…"
					className="flex-1 rounded border border-border bg-background px-1.5 py-0.5 font-mono text-[10px] placeholder:text-muted-foreground/60 focus:outline-none focus:ring-1 focus:ring-ring/50"
				/>
				<button
					type="button"
					onClick={() => refresh(filter)}
					disabled={loading}
					title="Refresh DOM tree"
					aria-label="Refresh DOM tree"
					className="flex h-5 w-5 items-center justify-center rounded text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground disabled:opacity-40"
				>
					<RefreshCw className={cn('h-3 w-3', loading && 'animate-spin')} />
				</button>
				{result && (
					<span className="font-mono text-[9px] uppercase tracking-[0.10em] text-muted-foreground">
						gen {result.generation}
					</span>
				)}
			</div>
			<div className="flex-1 min-h-0 overflow-auto p-2 font-mono text-[11px] leading-snug">
				{error ? (
					<div className="text-destructive">DOM probe failed: {error}</div>
				) : !result ? (
					<div className="text-muted-foreground">{loading ? 'Probing…' : 'No data yet.'}</div>
				) : result.text.length === 0 ? (
					<div className="text-muted-foreground">
						Empty tree.{' '}
						{filter ? 'No matches for the current filter.' : 'Iframe may still be loading.'}
					</div>
				) : (
					<pre className="whitespace-pre text-foreground/90">{result.text}</pre>
				)}
			</div>
		</div>
	);
}
