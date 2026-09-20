// Dispatch target resolution — spec §5.3, ADR-021.
//
//   resolveTarget(target) → { send(text, context) }
//
// Two implementations behind one shape:
//   • PTY inject — a `session` target with a live PTY: write the text into
//     that PTY (`ptyWrite`, the same command `POST /iyke/terminal/send` uses),
//     with the context PREPENDED as a comment line so a human reading the
//     terminal sees where the text came from.
//   • Chi runtime — a `session` target with no live PTY (a headless Chi run)
//     resumes the run (`chiResume`); a `new` / `persistent` target starts one
//     (`chiRun`). The context is APPENDED to the prompt.
//
// `send` is fire-and-forget by contract (ADR-021 checklist): its promise
// settles when the host accepted the text, never with a response to render.
// It resolves to `void` so no caller can grow a "response" slot from it.

import type { CompanionTarget } from '@/lib/shell/shell-store';
import { useShellStore } from '@/lib/shell/shell-store';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { PaneNode, PaneView } from '@/lib/panes/types';
import { chiResume, chiRun, ptyWrite } from '@/lib/tauri-cmd';
import { getPty } from '@/terminal/pty-registry';
import { useTerminalStore } from '@/terminal/session-store';

/** §5.3 `context` — where the dispatch came from. */
export interface DispatchContext {
	/** Active project root (or id when the project has no folder). */
	project?: string | null;
	/** The focused pane's current view (route or path). */
	focusedView?: string | null;
	/** Selected text / handed-off item, when there is one. */
	selection?: string | null;
}

export type ResolvedKind = 'pty' | 'chi-resume' | 'chi-run' | 'none';

export interface ResolvedTarget {
	kind: ResolvedKind;
	/** Engine a chi run starts on (chi-run only). */
	engineId?: string | null;
	/** Why nothing resolves — the dispatch input's disabled `title`. */
	disabledReason?: string;
	send: (text: string, context?: DispatchContext) => Promise<void>;
}

export const NO_ENGINE_REASON = 'No engine installed — open Ngwa → Store';

function contextParts(ctx?: DispatchContext): string[] {
	if (!ctx) return [];
	const parts: string[] = [];
	if (ctx.project) parts.push(`project ${ctx.project}`);
	if (ctx.focusedView) parts.push(`view ${ctx.focusedView}`);
	if (ctx.selection) parts.push(`selection ${ctx.selection}`);
	return parts;
}

/** One-line comment prepended to a PTY write. Newlines are flattened so the
 *  comment can never become a second command. */
export function contextCommentLine(ctx?: DispatchContext): string | null {
	const parts = contextParts(ctx);
	if (parts.length === 0) return null;
	return `# ikenga · ${parts.join(' · ')}`.replace(/[\r\n]+/g, ' ');
}

/** Prompt with the context appended (chi_run / chi_resume). */
export function promptWithContext(text: string, ctx?: DispatchContext): string {
	const parts = contextParts(ctx);
	if (parts.length === 0) return text;
	return `${text}\n\nContext: ${parts.join('; ')}`;
}

/** Terminals that run an agent TUI rather than a shell (a `wrap` spec). A
 *  leading `# …` line typed into an agent's prompt is not a comment — Claude
 *  Code, for one, reads it as an instruction — so the context line is only
 *  prepended for plain shells. */
function isAgentTerminal(sessionId: string): boolean {
	const tab = useTerminalStore.getState().tabs.find((t) => t.id === sessionId);
	return Boolean(tab?.spec.wrap);
}

/** The core PTY id for a terminal session, if its PTY is live. */
function livePtyFor(sessionId: string): { ptyId: string; persistent: boolean } | null {
	const pty = getPty(sessionId);
	if (pty && !pty.exited) return { ptyId: pty.id, persistent: pty.mode === 'persistent' };
	const tab = useTerminalStore.getState().tabs.find((t) => t.id === sessionId);
	if (tab?.ptyId && tab.status === 'running') {
		return { ptyId: tab.ptyId, persistent: tab.mode === 'persistent' };
	}
	return null;
}

function engineFor(engineId: string | null): string | null {
	return engineId ?? useShellStore.getState().defaultEngineId ?? null;
}

function chiRunTarget(engineId: string | null, persistent: boolean): ResolvedTarget {
	const engine = engineFor(engineId);
	if (!engine) {
		return {
			kind: 'none',
			disabledReason: NO_ENGINE_REASON,
			send: async () => {
				throw new Error(NO_ENGINE_REASON);
			},
		};
	}
	return {
		kind: 'chi-run',
		engineId: engine,
		send: async (text, context) => {
			const cwd = useShellStore.getState().activeProject.root_path;
			const result = await chiRun({
				engineId: engine,
				prompt: promptWithContext(text, context),
				...(cwd ? { cwd } : {}),
				persistent,
			});
			if (result.status === 'failed' && result.error) throw new Error(result.error);
		},
	};
}

export function resolveTarget(target: CompanionTarget): ResolvedTarget {
	switch (target.kind) {
		case 'session': {
			const sessionId = target.session_id;
			if (livePtyFor(sessionId)) {
				return {
					kind: 'pty',
					send: async (text, context) => {
						// Re-resolve at send time: the PTY may have exited or respawned.
						const live = livePtyFor(sessionId);
						if (!live) throw new Error('That terminal is no longer running');
						const comment = isAgentTerminal(sessionId) ? null : contextCommentLine(context);
						const data = `${comment ? `${comment}\r` : ''}${text}\r`;
						if (live.persistent) {
							// Persistent PTYs live in the daemon; the Pty object routes
							// the write there (and falls back to `ptyWrite` itself).
							const pty = getPty(sessionId);
							if (pty) return pty.write(data);
						}
						await ptyWrite(live.ptyId, data);
					},
				};
			}
			return {
				kind: 'chi-resume',
				send: async (text, context) => {
					const result = await chiResume(sessionId, promptWithContext(text, context));
					if (result.status === 'failed' && result.error) throw new Error(result.error);
				},
			};
		}
		case 'new':
			return chiRunTarget(target.engine_id, false);
		case 'persistent':
			return chiRunTarget(target.engine_id, true);
	}
}

function findLeaf(node: PaneNode, id: string): Extract<PaneNode, { type: 'leaf' }> | null {
	if (node.type === 'leaf') return node.id === id ? node : null;
	for (const child of node.children) {
		const found = findLeaf(child, id);
		if (found) return found;
	}
	return null;
}

function describeView(view: PaneView | undefined): string | null {
	if (!view) return null;
	switch (view.kind) {
		case 'route':
			return view.path || '/';
		case 'terminal':
			return `terminal ${view.sessionId}`;
		case 'scratchpad':
			return `scratchpad ${view.name}`;
		default:
			return view.path;
	}
}

/** The §5.3 context for a dispatch made right now. */
export function currentDispatchContext(selection?: string | null): DispatchContext {
	const project = useShellStore.getState().activeProject;
	const { root, focusedId } = usePaneStore.getState();
	const leaf = findLeaf(root, focusedId);
	return {
		project: project.root_path ?? (project.id !== 'default' ? project.id : null),
		focusedView: describeView(leaf?.tabs[leaf.activeTabIdx]),
		selection: selection ?? null,
	};
}
