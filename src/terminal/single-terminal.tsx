import { useEffect, useRef, useState } from 'react';
import { defaultShellArgv } from '@/lib/platform';
import { activeProjectCwd } from '@/lib/shell/active-project-cwd';
import { Pty } from './pty-bridge';
import { attachCapture } from './pty-output-buffer';
import { disposePty, getPty, registerPty } from './pty-registry';
import { useTerminalStore } from './session-store';
import { XTermHost } from './xterm-host';

interface SingleTerminalProps {
	sessionId: string;
	/** Whether the pane hosting this terminal currently has focus. Threaded
	 *  through to `XTermHost` so a cache-hit remount (see xterm-host.tsx's
	 *  module-scope xterm cache) only steals DOM focus when the user is
	 *  actually looking at this pane. Optional — callers that don't track
	 *  pane focus (e.g. Studio's terminal mount) simply never auto-focus on
	 *  a reparent, matching today's behavior for that call site. */
	isFocused?: boolean;
	/**
	 * T-3a (reclaim half of T-2, plans/multi-window): opt-in, one-shot repaint
	 * nudge for THIS mount, forwarded verbatim to `XTermHost`'s `nudgeOnAttach`
	 * (see xterm-host.tsx for the wobble itself). Only `terminal-view.tsx`
	 * passes `true`, and only on the render where a detached surface was just
	 * reclaimed — every other caller (Studio's terminal mount included) omits
	 * it, so it defaults to `undefined`/falsy and this prop changes nothing
	 * for them.
	 */
	nudgeOnAttach?: boolean;
}

// Hosts exactly one PTY inside a pane tab. The session record (cwd, cmd,
// title, status) lives in the terminal-store; the live PTY lives in the
// module-level registry so it survives pane-tree remounts.
export function SingleTerminal({ sessionId, isFocused, nudgeOnAttach }: SingleTerminalProps) {
	const tab = useTerminalStore((s) => s.tabs.find((t) => t.id === sessionId));
	const setStatus = useTerminalStore((s) => s.setStatus);
	const setPtyId = useTerminalStore((s) => s.setPtyId);

	const [pty, setPty] = useState<Pty | null>(() => getPty(sessionId) ?? null);
	const startedRef = useRef(false);

	// Spawn lifecycle: if the tab is in 'spawning' status with no live PTY,
	// call Pty.spawn(). On exit, update store status. Stays idempotent across
	// remounts via the registry + startedRef guard.
	useEffect(() => {
		if (!tab) return;
		if (startedRef.current) return;
		if (pty) return;
		if (tab.status !== 'spawning') return;
		startedRef.current = true;
		let cancelled = false;
		(async () => {
			try {
				const p = await Pty.spawn({
					terminalId: sessionId,
					title: tab.title,
					cwd: tab.spec.cwd,
					cmd: tab.spec.cmd,
					env: tab.spec.env,
					label: tab.spec.cmd.join(' '),
				});
				if (cancelled) {
					await p.dispose().catch(() => {});
					return;
				}
				p.onExit((code) => {
					// Drop the dead PTY from the registry so a click-to-respawn finds
					// a clean slate.
					disposePty(sessionId);
					setStatus(sessionId, 'exited', code);
				});
				registerPty(sessionId, p);
				// Tee PTY bytes into a per-session ring buffer so iyke can read
				// the visible/scrollback content without screenshotting xterm's
				// canvas. Lifetime is tied to the PTY via the registry's dispose.
				attachCapture(sessionId, p);
				setPty(p);
				setPtyId(sessionId, p.id);
				setStatus(sessionId, 'running');
			} catch (err) {
				console.error('[single-terminal] spawn failed', err);
				setStatus(sessionId, 'error');
			}
		})();
		return () => {
			cancelled = true;
		};
	}, [tab, pty, sessionId, setPtyId, setStatus]);

	// Allow respawn — when status flips back to 'spawning' (manual respawn),
	// reset the spawn guard so the effect above takes another shot.
	useEffect(() => {
		if (tab?.status === 'spawning') {
			startedRef.current = false;
			if (pty?.exited || !pty) setPty(getPty(sessionId) ?? null);
		}
	}, [tab?.status, pty, sessionId]);

	if (!tab) {
		return <Centered text={`Terminal session ${sessionId.slice(0, 8)}… not found.`} />;
	}
	if (!pty) {
		if (tab.status === 'exited' || tab.status === 'error') {
			return (
				<Centered>
					{tab.status === 'error'
						? `Failed to spawn: ${tab.spec.cmd.join(' ')}`
						: `Terminal exited (code=${tab.exitCode ?? '?'}).`}
					<br />
					<button
						type="button"
						onClick={() => {
							startedRef.current = false;
							setStatus(sessionId, 'spawning');
						}}
						className="mt-2 rounded-md border border-border bg-background px-3 py-1 text-xs hover:bg-accent"
					>
						Restart <code className="ml-1 font-mono">{tab.spec.cmd.join(' ')}</code>
					</button>
				</Centered>
			);
		}
		return <Centered text={`Spawning ${tab.spec.cmd.join(' ')}…`} />;
	}
	return (
		<XTermHost pty={pty} sessionId={sessionId} focused={isFocused} nudgeOnAttach={nudgeOnAttach} />
	);
}

interface CenteredProps {
	text?: string;
	className?: string;
	children?: React.ReactNode;
}

function Centered({ text, className, children }: CenteredProps) {
	return (
		<div
			className={`flex h-full w-full items-center justify-center bg-background p-6 text-center text-xs text-muted-foreground ${className ?? ''}`}
		>
			<div>{text ?? children}</div>
		</div>
	);
}

// Helper to create a new terminal session and return its id. Caller wires
// it into a pane tab via paneStore.addTab(focusedId, { kind: 'terminal',
// sessionId: id }).
export function createTerminalSession(opts?: {
	cwd?: string;
	cmd?: string[];
	title?: string;
}): string {
	const cwd = opts?.cwd ?? activeProjectCwd();
	const cmd = opts?.cmd ?? defaultShellArgv();
	return useTerminalStore.getState().add({ cwd, cmd }, opts?.title);
}
