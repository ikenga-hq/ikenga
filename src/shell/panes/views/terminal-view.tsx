// Side-pane Terminal panel entry point.
//
// Gates ownership: if the tab is currently attached to an Artifact Studio
// loupe, the side pane keeps its tab strip entry but the panel body shows
// a placeholder pointing at the owning Studio (D4). `SingleTerminal` stays
// ownership-agnostic so Studio can mount it directly without the gate.

import { Activity, ArrowUpRight, ExternalLink, GitBranch, History, ShieldAlert, Undo2 } from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';
import { Button } from '@/components/ui/button';
import { FeedbackState } from '@/components/ui/feedback-state';
import { IconButton } from '@/components/ui/icon-button';
import { findLeaf, getActiveView } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import { spawnWindow } from '@/lib/tauri-cmd';
import {
	clearPendingReclaimNudge,
	hasPendingReclaimNudge,
	markSurfaceDetached,
	syncDetachedSurfaces,
	useIsSurfaceDetached,
} from '@/lib/window/detached-surfaces';
import { CostHud } from '@/terminal/cost-hud';
import { GitLedger } from '@/terminal/git-ledger';
import { PermissionInbox } from '@/terminal/permission-inbox';
import { type TerminalTab, useTerminalStore } from '@/terminal/session-store';
import { SingleTerminal } from '@/terminal/single-terminal';
import { ToolCallFeed } from '@/terminal/tool-call-feed';
import { TranscriptReplay } from '@/terminal/transcript-replay';
import { DetachedSurfacePlaceholder } from './detached-placeholder';

interface TerminalViewProps {
	sessionId: string;
}

export function TerminalView({ sessionId }: TerminalViewProps) {
	const tab = useTerminalStore((s) => s.tabs.find((t) => t.id === sessionId));

	// Is the pane currently hosting THIS terminal session the focused pane?
	// Threaded down to XTermHost so a cache-hit remount (see xterm-host.tsx)
	// only steals DOM focus when the user is actually looking at this pane —
	// terminal-view.tsx doesn't receive its own paneId as a prop, so this
	// resolves it by searching the pane tree for the focused leaf's active
	// view instead of threading a new prop through pane-views.tsx.
	const isFocused = usePaneStore((s) => {
		const leaf = findLeaf(s.root, s.focusedId);
		if (!leaf) return false;
		const active = getActiveView(leaf);
		return active.kind === 'terminal' && active.sessionId === sessionId;
	});

	// Pop-out: spawn a thin single-surface window that ATTACHES to this
	// terminal's live core PTY. Encodes the real PTY id (not the pane session
	// id) in the surface_set so the detached TerminalSurface can attach over
	// the shared `pty://<id>` stream. (plans/multi-window WP-08.)
	const ptyId = tab?.ptyId ?? null;
	const surfaceId = ptyId ? `terminal:${ptyId}` : null;
	const isDetached = useIsSurfaceDetached(surfaceId);

	// T-3a (reclaim half of T-2, plans/multi-window "corruption 2 (reflow)"):
	// a reclaim (detached window closes, this pane remounts the live
	// terminal below) hits the identical missing-SIGWINCH failure mode T-2
	// fixed for pop-out — the remounted XTermHost fits to a size that may
	// already equal the PTY's current winsize, so no SIGWINCH is generated
	// and a full-screen TUI (vim, htop, claude itself) stays visually
	// corrupted even though the byte stream is correct. `nudgeOnAttach`
	// (xterm-host.tsx) already does the right thing for this; the only gap
	// was arming it ONLY on a genuine reclaim — never on an ordinary tab
	// switch, pane move/split, or cache-hit remount, all of which also
	// remount XTermHost (harmlessly, via its module-scope cache) and must
	// NOT get an extra wobble.
	//
	// `isDetached` (backed by `useIsSurfaceDetached`) is level-only — it has
	// no transition marker of its own — so the true→false edge is detected
	// here by comparing against the previous render's value via the
	// React-documented "adjust state during render" shape. That comparison
	// is a pure read (safe under React StrictMode's double-invoked render);
	// the actual consumption of the shared `pendingReclaimNudge` flag is
	// split into a peek (`hasPendingReclaimNudge`, read here, during render)
	// and a clear (`clearPendingReclaimNudge`, in the effect below, which
	// runs after commit and is idempotent) so a StrictMode double-invoke of
	// the effect can't silently eat the flag before the mount that needs it
	// ever sees it.
	//
	// NOTE: this does NOT repair bug (B) — the cached offscreen Terminal
	// ingested the entire pop-out's byte stream at the detached window's
	// stale geometry, so its scrollback stays permanently mis-wrapped. A
	// resize nudge can only force a repaint of the current screen; it cannot
	// rewrite already-written scrollback cells.
	const [reclaimGate, setReclaimGate] = useState(() => ({
		wasDetached: isDetached,
		justReclaimed: false,
	}));
	if (reclaimGate.wasDetached !== isDetached) {
		setReclaimGate({
			wasDetached: isDetached,
			justReclaimed: reclaimGate.wasDetached && !isDetached,
		});
	}
	const nudgeOnAttach =
		reclaimGate.justReclaimed && surfaceId ? hasPendingReclaimNudge(surfaceId) : false;
	useEffect(() => {
		if (reclaimGate.justReclaimed && surfaceId) clearPendingReclaimNudge(surfaceId);
	}, [reclaimGate.justReclaimed, surfaceId]);

	const handlePopOut = useCallback(() => {
		if (!ptyId || !surfaceId) return;
		const label = `detached-terminal-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
		// Optimistically mark detached so this pane swaps to the placeholder
		// immediately instead of briefly duplicating the live terminal.
		markSurfaceDetached(surfaceId, label);
		void spawnWindow({
			label,
			kind: 'single-surface',
			surface_set: [surfaceId],
			project_id: null,
			layout_key: label,
		}).catch((e) => {
			console.warn('pop-out terminal:', e);
			// Reconcile the optimistic mark if the window never opened.
			void syncDetachedSurfaces();
		});
	}, [ptyId, surfaceId]);

	if (tab && tab.owner.kind === 'studio') {
		const ownerPaneId = tab.owner.paneId;
		return (
			<StudioOwnedPlaceholder
				tab={tab}
				paneId={ownerPaneId}
				artifactPath={tab.owner.artifactPath}
				onReclaim={() => useTerminalStore.getState().detachFromStudio(sessionId)}
				onOpenStudio={() => usePaneStore.getState().focusPane(ownerPaneId)}
			/>
		);
	}

	const [sidePaneMode, setSidePaneMode] = useState<'none' | 'replay' | 'feed' | 'ledger' | 'permissions'>('none');

	// Popped out into its own window — render the reclaim placeholder, not the
	// live duplicate (both windows would otherwise drive the same core PTY).
	if (isDetached && surfaceId) {
		return <DetachedSurfacePlaceholder surfaceId={surfaceId} noun="terminal" />;
	}

	const togglePane = (mode: 'replay' | 'feed' | 'ledger' | 'permissions') => {
		setSidePaneMode((prev) => (prev === mode ? 'none' : mode));
	};

	return (
		<div className="relative flex h-full w-full flex-col overflow-hidden">
			{/* WP-03: Cost & Context Telemetry HUD */}
			<CostHud sessionId={sessionId} />

			<div className="relative flex-1 flex h-full w-full overflow-hidden">
				<div className="absolute right-1.5 top-1.5 z-50 flex items-center gap-1">
					<IconButton
						onClick={() => togglePane('replay')}
						title={sidePaneMode === 'replay' ? 'Hide Replay' : 'Show Transcript Replay (WP-05)'}
						aria-label="Toggle Replay"
						className={sidePaneMode === 'replay' ? 'bg-purple-950/80 text-purple-300 border border-purple-700/50' : ''}
					>
						<History className="h-3.5 w-3.5" />
					</IconButton>

					<IconButton
						onClick={() => togglePane('feed')}
						title={sidePaneMode === 'feed' ? 'Hide Feed' : 'Show Live Tool-Call Feed (WP-08)'}
						aria-label="Toggle Tool Feed"
						className={sidePaneMode === 'feed' ? 'bg-sky-950/80 text-sky-300 border border-sky-700/50' : ''}
					>
						<Activity className="h-3.5 w-3.5" />
					</IconButton>

					<IconButton
						onClick={() => togglePane('ledger')}
						title={sidePaneMode === 'ledger' ? 'Hide Git Ledger' : 'Show Git Change Ledger (WP-09)'}
						aria-label="Toggle Git Ledger"
						className={sidePaneMode === 'ledger' ? 'bg-emerald-950/80 text-emerald-300 border border-emerald-700/50' : ''}
					>
						<GitBranch className="h-3.5 w-3.5" />
					</IconButton>

					<IconButton
						onClick={() => togglePane('permissions')}
						title={sidePaneMode === 'permissions' ? 'Hide Permissions' : 'Show Permission Inbox (WP-10)'}
						aria-label="Toggle Permissions"
						className={sidePaneMode === 'permissions' ? 'bg-amber-950/80 text-amber-300 border border-amber-700/50' : ''}
					>
						<ShieldAlert className="h-3.5 w-3.5" />
					</IconButton>

					{ptyId && (
						<IconButton
							onClick={handlePopOut}
							title="Pop out — open this terminal in a detached window"
							aria-label="Pop out terminal"
						>
							<ArrowUpRight className="h-3.5 w-3.5" />
						</IconButton>
					)}
				</div>

				<div className="flex-1 h-full relative">
					<SingleTerminal sessionId={sessionId} isFocused={isFocused} nudgeOnAttach={nudgeOnAttach} />
				</div>

				{/* Telemetry & Control Side Panes */}
				{sidePaneMode !== 'none' && (
					<div className="w-1/2 h-full border-l border-border/50 bg-zinc-950">
						{sidePaneMode === 'replay' && <TranscriptReplay sessionId={sessionId} />}
						{sidePaneMode === 'feed' && <ToolCallFeed sessionId={sessionId} />}
						{sidePaneMode === 'ledger' && <GitLedger sessionId={sessionId} />}
						{sidePaneMode === 'permissions' && <PermissionInbox sessionId={sessionId} />}
					</div>
				)}
			</div>
		</div>
	);
}

interface StudioOwnedPlaceholderProps {
	tab: TerminalTab;
	paneId: string;
	artifactPath: string;
	onReclaim: () => void;
	onOpenStudio: () => void;
}

function StudioOwnedPlaceholder({
	tab,
	paneId,
	artifactPath,
	onReclaim,
	onOpenStudio,
}: StudioOwnedPlaceholderProps) {
	const filename = artifactPath.split('/').filter(Boolean).pop() ?? artifactPath;
	return (
		<FeedbackState
			variant="empty"
			fill
			icon={ExternalLink}
			heading={tab.title}
			body={
				<span className="flex flex-col items-center gap-1">
					<span className="font-mono text-[10px] uppercase tracking-[0.14em]">
						In Studio · pane {paneId.slice(0, 6)}
					</span>
					<span className="font-mono text-[11px]" title={artifactPath}>
						attached to {filename}
					</span>
				</span>
			}
			action={
				<>
					<Button size="sm" variant="outline" onClick={onOpenStudio} className="h-7 px-3 text-xs">
						<ExternalLink className="mr-1 h-3 w-3" />
						Open Studio
					</Button>
					<Button size="sm" onClick={onReclaim} className="h-7 px-3 text-xs">
						<Undo2 className="mr-1 h-3 w-3" />
						Reclaim
					</Button>
				</>
			}
		/>
	);
}
