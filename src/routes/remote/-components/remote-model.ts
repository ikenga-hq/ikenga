// Pure helpers for the remote client (G-ACCESS §3.12, §5.7; D-05
// `remote-client`). WP-74b.

import { TIER_LABELS } from '@/lib/access/caps.gen';
import type { AccessStatus } from '@/lib/access/client';
import type {
	ChiCacheRow,
	ForegroundProcess,
	NotificationRow,
	TerminalDescriptor,
} from '@/lib/tauri-cmd';

/** §5.7: the post-hook annotates permission rows (WP-75). Absent until then. */
export type AnnotatedRow = NotificationRow & {
	can_decide?: boolean;
	waiting_on?: null | 'owner' | 'device' | 'approve';
};

/**
 * Why a permission card is read-only on this device, or `null` when it is
 * live. A card is live only when its row says `can_decide` (§3.12, §5.7;
 * D-7: a `dispatch` device sees the inbox read-only with its reason).
 */
export function inboxReadOnlyReason(row: AnnotatedRow, status: AccessStatus): string | null {
	if (row.resolvedAt) return 'Already answered';
	const tier = TIER_LABELS[status.credential.tier].label;
	if (row.can_decide === true) return null;
	switch (row.waiting_on) {
		case 'owner':
			return 'Waiting on the Owner';
		case 'device':
			return 'Answer on the device chosen for asks (this device only)';
		case 'approve':
			return `This device can't approve — it is ${tier}`;
		default:
			break;
	}
	if (!status.caps.includes('approve')) return `This device can't approve — it is ${tier}`;
	return 'Answer this on the computer for now';
}

// ── Dispatch targets ─────────────────────────────────────────────────────────
//
// The dispatch bar types into a terminal with `pty_write(text + '\r')`. Into a
// plain shell that runs the message as a command, so a terminal is a target
// only while its foreground process is an agent CLI; Chi runs take the text as
// a `chi_resume` follow-up prompt instead. Plain shells are never targets.

/** Agent CLIs the dispatch bar may type into. */
export const AGENT_CLIS = ['claude', 'codex', 'agy', 'opencode', 'pi'] as const;
export type AgentCli = (typeof AGENT_CLIS)[number];

/** Runtimes an agent CLI is commonly launched through (`node …/cli.js`). */
const WRAPPER_RUNTIMES = new Set(['node', 'nodejs', 'bun', 'deno']);
/** Package launchers whose first positional argument is the package to run. */
const PACKAGE_LAUNCHERS = new Set(['npx', 'bunx', 'pnpx']);

/** Package-path fragments that identify an agent CLI's entry script. */
const PACKAGE_HINTS: ReadonlyArray<[string, AgentCli]> = [
	['/@anthropic-ai/claude-code/', 'claude'],
	['/@openai/codex/', 'codex'],
	['/opencode-ai/', 'opencode'],
	['/pi-coding-agent/', 'pi'],
];

/** npm package names of the agent CLIs (for `npx <pkg>`). */
const PACKAGE_NAMES: Readonly<Record<string, AgentCli>> = {
	'@anthropic-ai/claude-code': 'claude',
	'@openai/codex': 'codex',
	'opencode-ai': 'opencode',
	'@mariozechner/pi-coding-agent': 'pi',
};

function basenameOf(token: string): string {
	const base = token.split(/[\\/]/).pop() ?? '';
	return base
		.toLowerCase()
		.replace(/^\./, '')
		.replace(/\.(m?js|cjs|ts|exe)$/, '');
}

/** An exact agent name, or the codex native binary (`codex-x86_64-…`). */
function agentFromName(token: string): AgentCli | null {
	const name = basenameOf(token);
	if ((AGENT_CLIS as readonly string[]).includes(name)) return name as AgentCli;
	if (name.startsWith('codex-')) return 'codex';
	return null;
}

function agentFromScript(token: string): AgentCli | null {
	const path = token.replace(/\\/g, '/').toLowerCase();
	for (const [hint, agent] of PACKAGE_HINTS) if (path.includes(hint)) return agent;
	return agentFromName(token);
}

/**
 * The agent CLI a terminal's foreground process is, or `null` for anything
 * else (a shell, an editor, an unknown process, no foreground). Fail-closed:
 * only the process name, or — for a node/bun/deno wrapper — the script it
 * runs (and an npx/bunx package after it), is matched; arbitrary later
 * arguments never are, so `bash -c claude` or `node build.js codex` is not an
 * agent.
 */
export function agentCliOf(fg: ForegroundProcess | null | undefined): AgentCli | null {
	if (!fg) return null;
	const direct = agentFromName(fg.name);
	if (direct) return direct;
	const runtime = basenameOf(fg.name);
	const argv0 = fg.args[0] ? basenameOf(fg.args[0]) : '';
	if (!WRAPPER_RUNTIMES.has(runtime) && !WRAPPER_RUNTIMES.has(argv0)) return null;
	// The positional arguments after the runtime itself (flags skipped; deno's
	// `run` subcommand skipped).
	const positional = fg.args.slice(1).filter((a) => !a.startsWith('-'));
	if (positional[0] === 'run' && (runtime === 'deno' || argv0 === 'deno')) positional.shift();
	const script = positional[0];
	if (!script) return null;
	if (PACKAGE_LAUNCHERS.has(basenameOf(script))) {
		// `npx @openai/codex@latest` — the package name, version dropped.
		const pkg = positional[1]?.replace(/(.)@[^/@]*$/, '$1').toLowerCase();
		if (!pkg) return null;
		return PACKAGE_NAMES[pkg] ?? (pkg.includes('/') ? null : agentFromName(pkg));
	}
	return agentFromScript(script);
}

export type DispatchTarget =
	| { kind: 'pty'; key: string; ptyId: string; agent: AgentCli; label: string }
	| { kind: 'chi'; key: string; runId: string; label: string };

export interface SessionRow {
	id: string;
	label: string;
	detail: string;
	tone: 'live' | 'muted' | 'warn';
	/** Where the dispatch bar may send to this session, or `null` when it may
	 *  not (a plain shell, or any terminal not running an agent CLI). */
	target: DispatchTarget | null;
}

/**
 * Live terminals first, then recent Chi runs. `foreground` is the served
 * `pty_foreground_snapshot`, keyed by PTY id; a terminal it lacks falls back
 * to the descriptor's own `foreground_command`.
 */
export function sessionRows(
	terms: TerminalDescriptor[],
	runs: ChiCacheRow[],
	foreground: Record<string, ForegroundProcess> = {}
): SessionRow[] {
	const rows: SessionRow[] = terms
		.filter((t) => t.status === 'running')
		.map((t) => {
			const fg = foreground[t.pty_id] ?? t.foreground_command;
			const label = t.label || t.title || t.argv.join(' ') || 'terminal';
			const agent = agentCliOf(fg);
			return {
				id: `pty:${t.pty_id}`,
				label,
				detail: fg?.name ? `live · ${fg.name}` : 'live',
				tone: 'live' as const,
				target: agent
					? {
							kind: 'pty' as const,
							key: `pty:${t.pty_id}`,
							ptyId: t.pty_id,
							agent,
							label: `${agent} · ${label}`,
						}
					: null,
			};
		});
	for (const r of runs.slice(0, 8)) {
		const running = r.status === 'running' || r.status === 'starting';
		const label = `${r.engine_id} · ${r.brief?.slice(0, 40) || r.run_id.slice(0, 8)}`;
		rows.push({
			id: `run:${r.run_id}`,
			label,
			detail: r.status,
			tone: running ? 'warn' : 'muted',
			target: { kind: 'chi', key: `run:${r.run_id}`, runId: r.run_id, label: `Chi · ${label}` },
		});
	}
	return rows;
}

/** Every session the dispatch bar may send to, in session order. */
export function dispatchTargets(rows: SessionRow[]): DispatchTarget[] {
	return rows.flatMap((r) => (r.target ? [r.target] : []));
}

export const NO_AGENT_TARGET = 'No agent is running — start one from the full app';

/** Why a terminal target is no longer safe to write to, or `null` when its
 *  foreground (re-read right before the write) is still an agent CLI. */
export function foregroundRefusal(
	target: Extract<DispatchTarget, { kind: 'pty' }>,
	fg: ForegroundProcess | null
): string | null {
	if (agentCliOf(fg)) return null;
	const now = fg?.name ? `\`${fg.name}\`` : 'nothing';
	return `Not sent — ${target.label} is no longer running an agent (it is running ${now} now). Pick a target again.`;
}

/** "Pixel 9 · Chrome · View + dispatch". */
export function credentialLine(status: AccessStatus): string {
	return TIER_LABELS[status.credential.tier].label;
}
