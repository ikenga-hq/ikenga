import { buildClaudeWrappedCmd, type AgentWrapOpts } from './claude-wrap';
import { getClaudeSettingsPathSync } from './claude-settings';
import type { TerminalTab } from './session-store';

/**
 * Build the arguments passed to `Pty.spawn()` from a persisted or live
 * terminal tab. Reconstructs the wrapped claude argv so a resume after
 * app restart adds `--resume <claudeSessionId>`.
 *
 * Kept in its own file so `session-store.ts` can respawn tabs from
 * `rehydrateFromDb` without depending on `single-terminal.tsx`.
 */
export function buildSpawnOpts(
	tab: TerminalTab,
	terminalId: string
): {
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

	if (tab.spec.wrap) {
		const wrap: AgentWrapOpts = {
			...tab.spec.wrap,
			terminalId,
			resumeSessionId: tab.claudeSessionId ?? null,
		};
		const cmd = buildClaudeWrappedCmd(wrap);
		const settingsPath =
			cmd.some((s) => s.includes('--settings')) &&
			(tab.spec.wrap.engine ?? 'claude') === 'claude' &&
			getClaudeSettingsPathSync(terminalId);
		return { ...base, cmd, settingsPath: settingsPath || undefined };
	}

	return { ...base, cmd: tab.spec.cmd };
}
