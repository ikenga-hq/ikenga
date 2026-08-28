// startArtifact — wires up the agent + Studio loupe handoff.
//
// Spawn a PTY with the chosen agent (claude / codex / gemini / custom), type
// the kickoff via bracketed paste, and mount it as a `terminal` pane. The
// Studio opens in grid density on the chosen folder alongside the agent pane;
// a file-watcher then swaps grid → loupe when the agent's first `.html` lands
// and runs the handoff prompt.
//
// The watcher outlives the wizard component — fire-and-forget side effect
// with a 30-minute self-timeout so it doesn't leak if the agent never
// writes anything.

import {
	fsListenWatch,
	fsMkdir,
	fsUnwatch,
	fsWatch,
	ptyWrite,
	type Project,
} from '@/lib/tauri-cmd';
import { findLeaf } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import { buildAgentWrappedCmd } from '@/terminal/claude-wrap';
import { createClaudeTerminalSession, createTerminalSession } from '@/terminal/single-terminal';
import { useTerminalStore } from '@/terminal/session-store';
import { type Archetype, slugifyName } from '@/shell/artifact-wizard/archetypes';
import { requestOrApplyHandoff } from '@/shell/artifact-wizard/handoff-pref';
import { useWizardPopStore } from '@/shell/artifact-wizard/pop-recovery-store';

export type AgentChoice =
	| { kind: 'claude' }
	| { kind: 'codex' }
	| { kind: 'gemini' }
	| { kind: 'custom'; cmd: string[] };

export interface StartArgs {
	project: Project;
	archetype: Archetype;
	/** Display name. Surfaced to the agent + slugified for the path hint. */
	name: string;
	/** Absolute folder path the agent is expected to write the artifact into.
	 *  Surfaced in the kickoff prompt + used as the prefix filter on the
	 *  project-root file watcher. The folder doesn't need to exist yet — the
	 *  agent may create it. */
	folder: string;
	/** Which CLI to spawn. claude / codex / gemini are well-known; `custom`
	 *  passes a user-supplied argv through. */
	agent: AgentChoice;
}

function resolveAgentCmd(agent: AgentChoice): {
	cmd: string[];
	title: string;
} {
	switch (agent.kind) {
		case 'claude':
			return { cmd: ['claude'], title: 'claude' };
		case 'codex':
			return { cmd: ['codex'], title: 'codex' };
		case 'gemini':
			return { cmd: ['gemini'], title: 'gemini' };
		case 'custom':
			return { cmd: agent.cmd, title: agent.cmd[0] ?? 'agent' };
	}
}

export interface StartResult {
	/** Set when the agent surface is a terminal pane. */
	terminalSessionId: string | null;
	slug: string;
	kickoffPrompt: string;
}

const WATCHER_TIMEOUT_MS = 30 * 60 * 1000;
const PTY_READY_TIMEOUT_MS = 10_000;
/** Delay between PTY-ready and prompt write. Gives the agent time to
 *  render its splash and reach an input-ready state before we type.
 *  claude / codex / gemini all show launch chrome and are equally
 *  forgiving of this default. */
const PROMPT_TYPING_DELAY_MS = 1500;
/** Delay between bracketed paste end and the submit Enter. Without this
 *  the TUI batches the Enter into the paste payload and treats it as
 *  another newline instead of a submit. */
const POST_PASTE_DELAY_MS = 200;
/** How long the watcher will hold off firing on a non-slug-matching `.html`
 *  while it waits for the agent's actual file. If the agent writes the
 *  expected file inside this window, the watcher pops the loupe on that
 *  instead. Tuned to be long enough to absorb a stray sibling create,
 *  short enough that the user isn't left staring at a blank grid. */
const FALLBACK_GRACE_MS = 5_000;

export async function startArtifact(args: StartArgs): Promise<StartResult> {
	const slug = slugifyName(args.name);
	const kickoffPrompt = args.archetype.kickoffPrompt({
		project: {
			display_name: args.project.display_name,
			root_path: args.project.root_path,
		},
		slug,
	});

	// Make sure the watched folder exists before the watcher arms — otherwise
	// a `<project>/<archetype-subdir>/` that doesn't exist yet would silently
	// miss the first create event because notify watches a non-existent path
	// as the parent dir, and our prefix filter wouldn't match the created
	// subdir entry. Idempotent. Errors are logged + soft-fail; the watcher
	// still runs against the project root.
	if (args.folder.length > 0) {
		await fsMkdir(args.folder).catch((e) => {
			console.warn('[wizard] mkdir', args.folder, 'failed (continuing):', e);
		});
	}

	// Layout: Studio on the left (active pane) in grid density pointed at the
	// chosen folder, terminal agent split off to the right. We re-focus the
	// Studio so the user can drive it without a click. When the watcher fires,
	// the Studio's view swaps grid → loupe in place; the agent pane stays
	// untouched on the right.
	const cwd = args.project.root_path ?? args.folder;
	const paneStore = usePaneStore.getState();
	const studioLeafId = paneStore.focusedId;
	paneStore.addTab(studioLeafId, {
		kind: 'artifact-studio',
		path: args.folder,
		density: 'grid',
	});

	paneStore.splitPane(studioLeafId, 'horizontal');
	const agentLeafId = usePaneStore.getState().focusedId;
	const terminalSessionId = mountTerminalAgent({
		agent: args.agent,
		cwd,
		archetypeLabel: args.archetype.label,
		agentLeafId,
	});
	void typeKickoff(terminalSessionId, kickoffPrompt);

	usePaneStore.getState().focusPane(studioLeafId);

	if (args.project.root_path) {
		void watchForArtifact({
			rootPath: args.project.root_path,
			folderPrefix: args.folder,
			studioLeafId,
			agentLeafId,
			terminalSessionId,
			slug,
		});
	}

	return { terminalSessionId, slug, kickoffPrompt };
}

// ─── Agent mounts ────────────────────────────────────────────────────────

function mountTerminalAgent(args: {
	agent: AgentChoice;
	cwd: string;
	archetypeLabel: string;
	agentLeafId: string;
}): string {
	const title = `${args.agent.kind} · ${args.archetypeLabel.toLowerCase()}`;
	let sessionId: string;

	if (args.agent.kind === 'claude') {
		sessionId = createClaudeTerminalSession({ cwd: args.cwd }, title);
	} else if (args.agent.kind === 'codex' || args.agent.kind === 'gemini') {
		const cmd = buildAgentWrappedCmd({ engine: args.agent.kind });
		sessionId = createTerminalSession({ cwd: args.cwd, cmd, title });
	} else {
		const { cmd } = resolveAgentCmd(args.agent);
		sessionId = createTerminalSession({ cwd: args.cwd, cmd, title });
	}

	usePaneStore.getState().addTab(args.agentLeafId, {
		kind: 'terminal',
		sessionId,
	});
	return sessionId;
}

/** Wait for the PTY id to land in the session-store, give the agent a beat
 *  to finish its splash, then bracket-paste the prompt and submit with a
 *  separate Enter keystroke.
 *
 *  Bracketed paste (`\x1b[200~ … \x1b[201~`) signals to the TUI input that
 *  the multi-line payload is a single pasted block — without it, embedded
 *  `\n`s fire intermediate newlines and the trailing CR is absorbed as
 *  another newline instead of a submit. The follow-up `\r` after a short
 *  delay then submits cleanly. Works across claude / codex / gemini. */
async function typeKickoff(sessionId: string, prompt: string): Promise<void> {
	const ptyId = await awaitPtyId(sessionId, PTY_READY_TIMEOUT_MS);
	if (!ptyId) {
		console.warn('[wizard] PTY never reported a ptyId — prompt not typed');
		return;
	}
	await wait(PROMPT_TYPING_DELAY_MS);
	try {
		await ptyWrite(ptyId, `\x1b[200~${prompt}\x1b[201~`);
		await wait(POST_PASTE_DELAY_MS);
		await ptyWrite(ptyId, '\r');
	} catch (e) {
		console.error('[wizard] ptyWrite failed:', e);
	}
}

function awaitPtyId(sessionId: string, timeoutMs: number): Promise<string | null> {
	return new Promise((resolve) => {
		const initial = useTerminalStore.getState().tabs.find((t) => t.id === sessionId)?.ptyId ?? null;
		if (initial) {
			resolve(initial);
			return;
		}
		const timer = setTimeout(() => {
			unsub();
			resolve(null);
		}, timeoutMs);
		const unsub = useTerminalStore.subscribe((state) => {
			const tab = state.tabs.find((t) => t.id === sessionId);
			if (tab?.ptyId) {
				clearTimeout(timer);
				unsub();
				resolve(tab.ptyId);
			}
		});
	});
}

function wait(ms: number): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Watch the project root recursively for the agent's file. Prefers any
 *  `.html` whose basename contains `slug` (case-insensitive); falls back
 *  to the first non-matching `.html` under `folderPrefix` after a short
 *  grace period so a stray sibling create doesn't pop the wrong file.
 *
 *  Swap sequence on fire: grid → loupe in place, then route the terminal
 *  pane through the handoff prompt / persisted pref. Self-terminates
 *  after 30 minutes if nothing lands. */
async function watchForArtifact(args: {
	rootPath: string;
	folderPrefix: string;
	studioLeafId: string;
	/** The leaf the agent terminal was mounted on. */
	agentLeafId: string;
	/** Set to the terminal session id when a handoff prompt should be typed. */
	terminalSessionId: string | null;
	slug: string;
}): Promise<void> {
	const { rootPath, folderPrefix, studioLeafId, agentLeafId, terminalSessionId, slug } = args;
	const normalizedPrefix = folderPrefix.replace(/\/+$/, '');
	const slugLower = slug.toLowerCase();
	let watcherId: string | null = null;
	let unlisten: (() => void) | null = null;
	let cleaned = false;
	/** First non-slug-matching `.html` we've seen, held in case the slug
	 *  match never lands. Falls through after FALLBACK_GRACE_MS. */
	let fallback: { path: string } | null = null;
	let fallbackTimer: ReturnType<typeof setTimeout> | null = null;

	function cleanup(): void {
		if (cleaned) return;
		cleaned = true;
		if (unlisten) {
			try {
				unlisten();
			} catch {}
			unlisten = null;
		}
		if (watcherId) {
			void fsUnwatch(watcherId).catch(() => {});
			watcherId = null;
		}
		if (fallbackTimer) {
			clearTimeout(fallbackTimer);
			fallbackTimer = null;
		}
	}

	const timeout = setTimeout(cleanup, WATCHER_TIMEOUT_MS);

	function fire(path: string, reason: 'slug-match' | 'fallback'): void {
		console.info('[wizard] artifact detected', path, `(${reason}) → swapping grid → loupe`);
		swapStudioToLoupe(studioLeafId, path);
		// Fallback fires might be the wrong file (a stray sibling create
		// landed before the agent's actual file). Post a recovery record so
		// the workspace-mounted chip offers the user one-click backtrack to
		// the folder grid. Slug-match fires skip this — they're almost
		// always the right file.
		if (reason === 'fallback') {
			useWizardPopStore.getState().post({
				paneId: studioLeafId,
				artifactPath: path,
				folder: normalizedPrefix,
				postedAt: Date.now(),
			});
		}
		// Prompt the user (or apply their persisted pref) for the right-pane
		// terminal handoff.
		if (terminalSessionId) {
			void requestOrApplyHandoff({
				terminalSessionId,
				terminalLeafId: agentLeafId,
				studioLeafId,
				artifactPath: path,
			}).catch((e) => console.warn('[wizard] handoff failed:', e));
		}
		clearTimeout(timeout);
		cleanup();
	}

	try {
		watcherId = await fsWatch(rootPath);
		console.info(
			'[wizard] watching',
			rootPath,
			'for .html under',
			normalizedPrefix,
			'(slug match:',
			slugLower,
			')'
		);
		unlisten = await fsListenWatch(watcherId, (change) => {
			if (cleaned) return;
			if (change.kind !== 'create') return;
			if (!change.path.toLowerCase().endsWith('.html')) return;
			if (!change.path.startsWith(`${normalizedPrefix}/`) && change.path !== normalizedPrefix) {
				return;
			}

			const basename = change.path
				.replace(/^.+\//, '')
				.replace(/\.html$/i, '')
				.toLowerCase();
			const isSlugMatch = slugLower.length > 0 && basename.includes(slugLower);

			if (isSlugMatch) {
				fire(change.path, 'slug-match');
				return;
			}

			// Non-matching create — hold as fallback. If the slug match
			// shows up before the grace window expires, that fire() will
			// cleanup() and the timer becomes a no-op via `cleaned`.
			if (!fallback) {
				fallback = { path: change.path };
				console.info(
					'[wizard] non-matching .html',
					change.path,
					'— holding as fallback for',
					FALLBACK_GRACE_MS,
					'ms'
				);
				fallbackTimer = setTimeout(() => {
					if (cleaned || !fallback) return;
					fire(fallback.path, 'fallback');
				}, FALLBACK_GRACE_MS);
			}
		});
	} catch (e) {
		console.warn('[wizard] could not watch', rootPath, e);
		clearTimeout(timeout);
	}
}

/** Swap the Studio leaf's active view from `density: 'grid'` (on the folder)
 *  to `density: 'loupe'` on the freshly-written file. Falls back to a new
 *  tab on the focused pane if the Studio leaf has been closed. */
function swapStudioToLoupe(studioLeafId: string, path: string): void {
	const ps = usePaneStore.getState();
	const view = { kind: 'artifact-studio' as const, path, density: 'loupe' as const };
	try {
		if (findLeaf(ps.root, studioLeafId)) {
			ps.replaceActiveViewAndPushHistory(studioLeafId, view);
		} else {
			ps.addTab(ps.focusedId, view);
		}
	} catch (e) {
		console.error('[wizard] auto-swap to loupe failed:', e);
	}
}
