import { listen } from '@tauri-apps/api/event';
import { useEffect, useRef, useState } from 'react';
import { defaultShellArgv } from '@/lib/platform';
import { activeProjectCwd } from '@/lib/shell/active-project-cwd';
import { getClaudeSettingsPathSync } from './claude-settings';
import { buildClaudeWrappedCmd, type AgentWrapOpts } from './claude-wrap';
import { type HookEventPayload } from './tool-call-feed';
import { Pty } from './pty-bridge';
import { attachCapture } from './pty-output-buffer';
import { disposePty, getPty, registerPty } from './pty-registry';
import { makeTerminalId, useTerminalStore, type TerminalTab } from './session-store';
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
	const startedRef = useRef(false);

	// Spawn / attach lifecycle. Idempotent across remounts via the registry +
	// startedRef guard. Three cases:
	//
	// 1. `tab.ptyId` set, no local pty — the PTY survived a refresh or was
	//    restored from DB. Attach to the live PTY.
	// 2. `tab.status === 'spawning'` — spawn a new PTY, rebuilding the claude
	//    argv with `--resume <claudeSessionId>` if we have one.
	// 3. Otherwise (exited/error) — render the placeholder / restart button.
	useEffect(() => {
		if (!tab) return;
		if (startedRef.current) return;
		if (pty) return;

		const shouldAttach = Boolean(tab.ptyId);
		const shouldSpawn = tab.status === 'spawning';
		if (!shouldAttach && !shouldSpawn) return;

		startedRef.current = true;
		let cancelled = false;

		(async () => {
			try {
				let p: Pty;
				if (shouldAttach && tab.ptyId) {
					p = await Pty.attach(tab.ptyId, tab.title);
				} else {
					const spawnOpts = buildSpawnOpts(tab, sessionId);
					p = await Pty.spawn(spawnOpts);
				}

				if (cancelled) {
					await p.dispose().catch(() => {});
					return;
				}

				p.onExit((code) => {
					// Drop the dead PTY from the registry so a click-to-respawn finds
					// a clean slate, and forget its resume id.
					disposePty(sessionId);
					setPtyId(sessionId, null);
					setClaudeSessionId(sessionId, null);
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
				console.error('[single-terminal] spawn/attach failed', err);
				setPtyId(sessionId, null);
				setStatus(sessionId, 'error');
			}
		})();

		return () => {
			cancelled = true;
		};
	}, [tab, pty, sessionId, setPtyId, setStatus, setClaudeSessionId]);

	// Allow respawn — when status flips back to 'spawning' (manual respawn),
	// reset the spawn guard so the effect above takes another shot.
	useEffect(() => {
		if (tab?.status === 'spawning') {
			startedRef.current = false;
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
		if (tab.status === 'exited' || tab.status === 'error') {
			return (
				<Centered>
					{tab.status === 'error'
						? `Failed to spawn: ${displayCmd(tab).join(' ')}`
						: `Terminal exited (code=${tab.exitCode ?? '?'}).`}
					<br />
					<button
						type="button"
						onClick={() => {
							startedRef.current = false;
							setPtyId(sessionId, null);
							setClaudeSessionId(sessionId, null);
							setStatus(sessionId, 'spawning');
						}}
						className="mt-2 rounded-md border border-border bg-background px-3 py-1 text-xs hover:bg-accent"
					>
						Restart <code className="ml-1 font-mono">{displayCmd(tab).join(' ')}</code>
					</button>
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
		return [tab.spec.wrap.engine ?? 'claude', ...(tab.spec.wrap.prompt ? [tab.spec.wrap.prompt] : [])];
	}
	return tab.spec.cmd;
}

function buildSpawnOpts(tab: TerminalTab, terminalId: string): {
	terminalId: string;
	title: string;
	cwd: string;
	cmd: string[];
	env?: Record<string, string>;
	label: string;
	settingsPath?: string;
} {
	const base = {
		terminalId,
		title: tab.title,
		cwd: tab.spec.cwd,
		env: tab.spec.env,
		label: tab.spec.cmd.join(' '),
	};

	// Claude terminals are wrapped in a shell script and have a per-terminal
	// `--settings` file. Rebuild the argv so a restart or respawn can add
	// `--resume` from the captured claude session id.
	if (tab.spec.wrap) {
		const wrap: AgentWrapOpts = {
			...tab.spec.wrap,
			terminalId,
			resumeSessionId: tab.claudeSessionId ?? null,
		};
		const cmd = buildClaudeWrappedCmd(wrap);
		// Only ask Rust to write the settings file if the argv actually carries
		// `--settings` and we can derive the deterministic per-terminal path.
		const settingsPath =
			cmd.some((s) => s.includes('--settings')) &&
			(tab.spec.wrap.engine ?? 'claude') === 'claude' &&
			getClaudeSettingsPathSync(terminalId);
		return { ...base, cmd, settingsPath: settingsPath || undefined };
	}

	return { ...base, cmd: tab.spec.cmd };
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
