// Detached terminal surface (plans/multi-window WP-08).
//
// Renders a live terminal in a thin detached window by ATTACHING to the
// existing core PTY — no activity-bar, sidebar, or pane-group chrome.
//
// The PTY lives in the shared Rust core; its `pty://<id>` data stream is
// broadcast to every window, so the detached window attaches its own xterm
// host to the same PTY (`Pty.attach`, non-owning) and both windows drive the
// same shell — keystrokes in either flow to the one PTY. Closing the detached
// window only detaches; the origin pane still owns + kills the PTY.
//
// Surface-set id convention: `"terminal:<ptyId>"` (the real PTY id, not the
// pane/session id), encoded by the pop-out in `pane/views/terminal-view.tsx`.
//
// WORKS (WP-08, live-verified 2026-06-28): pop out a terminal → this window
// attaches to the shared PTY and renders live; a command run in the origin
// terminal streams here (both windows drive the same shell). KEY FIX: the
// detached terminal must use the CANVAS renderer — WebGL "loads" in a secondary
// WebKitGTK webview but paints no glyphs (only the cursor) and never fires
// onContextLoss, so the terminal stays blank. We pass `disableWebgl` to
// XTermHost below. Scrollback: `Pty.attach` replays the origin PTY's recent
// output (Rust ring, ≤256KB) into xterm before the live stream, so the popped-
// out terminal no longer starts blank. Since T-1 that replay is handed over by
// an atomic snapshot-and-subscribe handshake in Rust (`pty_attach_begin` gates
// the stream, `pty_attach_arm` releases it), so the seam between the replayed
// tail and the live stream drops nothing and repeats nothing. Caveat: the
// replayed tail is raw bytes
// against a fresh screen (like any terminal reattach) — a few stale escape
// sequences may flicker at the top on first paint; live output is correct.
//
// T-2 (SIGWINCH-on-attach, corruption "reflow"): `nudgeOnAttach` below makes
// XTermHost wobble the PTY size by one column after its first fit(), forcing
// a real SIGWINCH so a full-screen TUI repaints at this window's geometry
// instead of the one it last drew at. See xterm-host.tsx's
// `scheduleAttachNudge` for why the wobble (not a plain resize) is required.
// This masks reflow for full-screen TUIs; it does not fix the raw-replay
// rewrap for line-mode shells (separate, out of scope — see plan).

// xterm's base stylesheet is otherwise imported only in boot/primary.tsx — a
// chunk the detached graph never loads — so the detached terminal would render
// without scroll/selection styling. Import it here, scoped to this lazy chunk.
import '@xterm/xterm/css/xterm.css';
import { isTauri } from '@/lib/transport';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Terminal } from 'lucide-react';
import { useEffect, useState } from 'react';

import { FeedbackState } from '@/components/ui/feedback-state';
import { Pty } from '@/terminal/pty-bridge';
import { useTerminalTitleByPtyId } from '@/terminal/use-terminal-titles';
import { XTermHost } from '@/terminal/xterm-host';

import type { DetachedSurfaceProps } from '../registry';

/** Extract the PTY id encoded in `"terminal:<ptyId>"` by the pop-out. */
function parsePtyId(surfaces: string[]): string | null {
	const entry = surfaces[0] ?? '';
	const colon = entry.indexOf(':');
	if (colon < 1) return null;
	const id = entry.slice(colon + 1);
	return id.length > 0 ? id : null;
}

export default function TerminalSurface({ ctx }: DetachedSurfaceProps) {
	const ptyId = parsePtyId(ctx.surfaces);
	const [pty, setPty] = useState<Pty | null>(null);
	const [error, setError] = useState<string | null>(null);

	// A popped-out terminal has no session-store entry — the store lives in the
	// origin window — so it names itself straight off the Rust descriptor,
	// joined by PTY id. Same `claude · shell` label as the origin tab, and it
	// tracks the foreground command live here too.
	const title = useTerminalTitleByPtyId(ptyId);

	// Every spawned window is built with the title "Ikenga", which makes a row
	// of popped-out terminals indistinguishable in the OS window list and the
	// alt-tab switcher. Name this one after what it's actually running.
	// Keyed on the label string, not the title object: the descriptor poll can
	// hand back a fresh object whose label is unchanged, and there's no reason
	// to re-cross the IPC boundary for that.
	const windowTitle = title?.label;
	useEffect(() => {
		if (!windowTitle || !isTauri()) return;
		void getCurrentWindow()
			.setTitle(`${windowTitle} — Ikenga`)
			.catch(() => {
				/* non-fatal: the window keeps its default title */
			});
	}, [windowTitle]);

	useEffect(() => {
		if (!ptyId) return;
		let cancelled = false;
		let attached: Pty | null = null;
		(async () => {
			try {
				const p = await Pty.attach(ptyId, `pty:${ptyId.slice(0, 8)}`);
				if (cancelled) {
					await p.dispose().catch(() => {});
					return;
				}
				attached = p;
				// Defer xterm mount until fonts + layout are settled. In a fresh
				// detached webview, xterm otherwise measures the character cell
				// before the monospace font/layout is ready → giant cells, ~1
				// column, invisible text. Waiting for fonts.ready + a layout frame
				// lets CharSizeService measure correctly.
				if (typeof document !== 'undefined' && document.fonts?.ready) {
					await document.fonts.ready;
				}
				await new Promise<void>((r) => requestAnimationFrame(() => r()));
				if (cancelled) {
					await p.dispose().catch(() => {});
					return;
				}
				setPty(p);
			} catch (e) {
				if (!cancelled) setError(e instanceof Error ? e.message : String(e));
			}
		})();
		return () => {
			cancelled = true;
			// Non-owning: detaches the listener, never kills the core PTY.
			attached?.dispose().catch(() => {});
		};
	}, [ptyId]);

	if (!ptyId) {
		return (
			<FeedbackState
				variant="empty"
				fill
				icon={Terminal}
				heading="No terminal"
				body="Open this window via the terminal pane pop-out button."
			/>
		);
	}
	if (error) {
		return <FeedbackState variant="error" fill heading="Terminal attach failed" body={error} />;
	}

	return (
		<div className="flex h-full w-full flex-col">
			<header
				className="flex shrink-0 items-center gap-2 border-b border-border bg-muted/20 px-3 py-1.5 font-mono text-[11px] text-muted-foreground"
				style={{ height: 'var(--tab-h, 32px)' }}
			>
				<Terminal className="h-3 w-3 shrink-0" />
				<span className="truncate" title={title?.tooltip ?? ptyId}>
					{title?.label ?? `terminal ${ptyId.slice(0, 8)}…`}
				</span>
			</header>
			<div className="min-h-0 flex-1">
				{pty ? (
					<XTermHost pty={pty} disableWebgl nudgeOnAttach />
				) : (
					<FeedbackState variant="loading" fill heading="Attaching…" />
				)}
			</div>
		</div>
	);
}
