import { listen } from '@/lib/transport';
import { useEffect, useState } from 'react';
import { TerminalSquare } from 'lucide-react';
import { EmptyState } from '@/components/states';
import { defaultShellArgv } from '@/lib/platform';
import { activeProjectCwd } from '@/lib/shell/active-project-cwd';
import { buildClaudeWrappedCmd, type AgentWrapOpts } from './claude-wrap';
import { type HookEventPayload } from './tool-call-feed';
import type { Pty } from './pty-bridge';
import { getPty } from './pty-registry';
import { makeTerminalId, openTabPty, useTerminalStore, type TerminalTab } from './session-store';
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
	const setClaudeSessionId = useTerminalStore((s) => s.setClaudeSessionId);

	const [pty, setPty] = useState<Pty | null>(() => getPty(sessionId) ?? null);
	const [sessionLost, setSessionLost] = useState(false);

	// Spawn / attach lifecycle. Three cases:
	//
	// 1. `tab.ptyId` set, no local pty — the PTY survived a refresh or was
	//    restored from DB. Attach to the live PTY.
	// 2. `tab.status === 'spawning'` — spawn a new PTY, rebuilding the claude
	//    argv with `--resume <claudeSessionId>` if we have one.
	// 3. Otherwise (exited/error) — render the placeholder / restart button.
	//
	// `openTabPty` is single-flight per tab and owns registration + exit
	// wiring, so this effect re-running (it depends on `tab`, which every
	// store write replaces) or racing the rehydrate auto-resume just joins the
	// same open. A cancelled run only skips `setPty`; it must NOT dispose the
	// PTY — the registry owns it, and the next run picks it up.
	useEffect(() => {
		if (!tab || pty) return;
		const shouldAttach = Boolean(tab.ptyId);
		if (!shouldAttach && tab.status !== 'spawning') return;

		let cancelled = false;
		openTabPty(tab)
			.then((p) => {
				if (!cancelled) setPty(p);
			})
			.catch((err) => {
				if (cancelled) return;
				console.error('[single-terminal] spawn/attach failed', err);
				setPtyId(sessionId, null);
				if (shouldAttach) setSessionLost(true);
				setStatus(sessionId, 'error');
			});

		return () => {
			cancelled = true;
		};
	}, [tab, pty, sessionId, setPtyId, setStatus]);

	// Manual respawn — when status flips back to 'spawning', clear the
	// lost-session flag and any exited PTY so the effect above opens a new one.
	useEffect(() => {
		if (tab?.status === 'spawning') {
			setSessionLost(false);
			setPtyId(sessionId, null);
			if (pty?.exited || !pty) setPty(getPty(sessionId) ?? null);
		}
	}, [tab?.status, pty, sessionId, setPtyId]);

	// Capture the claude session id from the SessionStart hook so we can resume
	// after a full app restart. Filter by `ikenga_terminal_id` so this terminal
	// only reacts to its own claude session.
	useEffect(() => {
		let unlisten: (() => void) | undefined;
		listen<HookEventPayload>('hooks://event', (event) => {
			const p = event.payload;
			if (!p || p.ikenga_terminal_id !== sessionId) return;

			if (p.hook_event_name === 'SessionStart' && p.session_id) {
				setClaudeSessionId(sessionId, p.session_id);
			} else if (p.hook_event_name === 'SessionEnd') {
				// The claude session itself ended; the PTY may keep going but we
				// no longer have a conversation to resume.
				setClaudeSessionId(sessionId, null);
			}
		})
			.then((fn) => {
				unlisten = fn;
			})
			.catch(() => {});

		return () => {
			if (unlisten) unlisten();
		};
	}, [sessionId, setClaudeSessionId]);

	if (!tab) {
		return <Centered text={`Terminal session ${sessionId.slice(0, 8)}… not found.`} />;
	}
	if (!pty) {
		if (tab.status === 'exited' || tab.status === 'error' || sessionLost) {
			const isLost = sessionLost || (tab.status === 'error' && tab.mode === 'persistent');
			const restart = () => {
				setSessionLost(false);
				setPtyId(sessionId, null);
				setClaudeSessionId(sessionId, null);
				setStatus(sessionId, 'spawning');
			};
			// Plain exit (not a lost session, not a spawn failure) is the D-07
			// `states` "terminal exited" cell — one action, restart.
			if (!isLost && tab.status !== 'error') {
				return (
					<EmptyState
						data-state="terminal-exited"
						icon={TerminalSquare}
						heading={`Process exited · code ${tab.exitCode ?? '?'}`}
						body="The scrollback is kept. Restarting reuses the same working directory and the same engine."
						fill
						action={{ label: `Restart ${displayCmd(tab).join(' ')}`, onClick: restart }}
					/>
				);
			}
			return (
				<Centered>
					<div className="flex flex-col items-center gap-2 max-w-sm">
						{isLost ? (
							<>
								<div className="text-sm font-semibold text-destructive">Session Lost</div>
								<div className="text-xs text-muted-foreground">
									The terminal session could not be reattached (the daemon may have restarted or the
									session was terminated).
								</div>
							</>
						) : (
							<div className="text-destructive">Failed to spawn: {displayCmd(tab).join(' ')}</div>
						)}
						<button
							type="button"
							onClick={restart}
							className="mt-2 rounded-md border border-border bg-background px-3 py-1.5 text-xs font-medium hover:bg-accent hover:text-accent-foreground transition-colors"
						>
							Restart <code className="ml-1 font-mono">{displayCmd(tab).join(' ')}</code>
						</button>
					</div>
				</Centered>
			);
		}
		return <Centered text={`Spawning ${displayCmd(tab).join(' ')}…`} />;
	}
	return (
		<XTermHost pty={pty} sessionId={sessionId} focused={isFocused} nudgeOnAttach={nudgeOnAttach} />
	);
}

function displayCmd(tab: TerminalTab): string[] {
	if (tab.spec.wrap) {
		// Show the user the resolved claude invocation (without the bash wrapper chrome).
		return [
			tab.spec.wrap.engine ?? 'claude',
			...(tab.spec.wrap.prompt ? [tab.spec.wrap.prompt] : []),
		];
	}
	return tab.spec.cmd;
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

// Helper to create a new plain terminal session and return its id. Caller wires
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

// Helper to create a new Claude terminal session with a stable id, so the
// per-terminal hook settings file can be determined before the PTY spawns.
export function createClaudeTerminalSession(
	opts: AgentWrapOpts = {},
	title = opts.prompt ? 'claude' : 'claude'
): string {
	const id = makeTerminalId();
	const cwd = opts.cwd ?? activeProjectCwd();
	const wrap: AgentWrapOpts = { ...opts, terminalId: id, resumeSessionId: null };
	const cmd = buildClaudeWrappedCmd(wrap);
	return useTerminalStore.getState().add({ cwd, cmd, wrap }, title, id);
}
