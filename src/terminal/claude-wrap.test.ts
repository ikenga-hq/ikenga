// Unit tests for buildAgentWrappedCmd & buildClaudeWrappedCmd — the argv builder behind every
// "open AI agent in a terminal" affordance.

import { afterEach, describe, expect, it } from 'vitest';

import { __setClaudeSettingsPathForTests } from './claude-settings';
import {
	buildAgentArgs,
	buildAgentEnv,
	buildAgentWrappedCmd,
	buildClaudeWrappedCmd,
} from './claude-wrap';
import { buildSpawnOpts } from './spawn-opts';
import type { TerminalTab } from './session-store';

/** Pull the quoted command invocation out of the bash wrapper script so
 *  assertions read against the real command rather than the printf chrome. */
function extractInvocation(cmd: string[]): string {
	const script = cmd.at(-1) ?? '';
	const runStart = script.indexOf('; ') + 2;
	const runEnd = script.indexOf('; __status=$?');
	return script.slice(runStart, runEnd);
}

describe('buildClaudeWrappedCmd & buildAgentWrappedCmd', () => {
	it('wraps the invocation in an interactive bash script for posix targets', () => {
		const cmd = buildClaudeWrappedCmd({ shellTarget: 'bash' });
		expect(cmd.slice(0, 3)).toEqual(['/bin/bash', '-i', '-c']);
		const script = cmd.at(-1) ?? '';
		expect(script).toContain('exec "${SHELL:-bash}" -i');
		expect(script).toContain('[claude exited');
	});

	it('wraps the invocation in PowerShell with ExecutionPolicy Bypass', () => {
		const cmd = buildClaudeWrappedCmd({ shellTarget: 'powershell' });
		expect(cmd[0]).toBe('powershell.exe');
		expect(cmd).toContain('-ExecutionPolicy');
		expect(cmd).toContain('Bypass');
		const script = cmd.at(-1) ?? '';
		expect(script).toContain("if (Get-Command 'claude'");
		expect(script).toContain('[claude exited');
	});

	it('wraps the invocation in pwsh when pwsh target is specified', () => {
		const cmd = buildClaudeWrappedCmd({ shellTarget: 'pwsh' });
		expect(cmd[0]).toBe('pwsh.exe');
		expect(cmd).toContain('-ExecutionPolicy');
		expect(cmd).toContain('Bypass');
	});

	it('wraps the invocation for WSL with distribution and cwd flags', () => {
		const cmd = buildClaudeWrappedCmd({
			shellTarget: 'wsl',
			wslDistro: 'Ubuntu',
			cwd: 'C:\\Users\\nedJamez\\project',
		});
		expect(cmd[0]).toBe('wsl.exe');
		expect(cmd).toContain('-d');
		expect(cmd).toContain('Ubuntu');
		expect(cmd).toContain('--cd');
		expect(cmd).toContain('C:/Users/nedJamez/project');
		// `-e` must precede bash, or the distro login shell expands `$__status`.
		expect(cmd.indexOf('-e')).toBe(cmd.indexOf('bash') - 1);
	});

	it('hands WSL claude a /mnt path for a Windows-side --settings file', () => {
		__setClaudeSettingsPathForTests('C:\\Users\\me\\AppData\\Local\\app.ikenga');
		const wsl = buildClaudeWrappedCmd({ shellTarget: 'wsl', terminalId: 't1' }).at(-1) ?? '';
		expect(wsl).toContain("'/mnt/c/Users/me/AppData/Local/app.ikenga/claude-hooks-t1.json'");
		expect(wsl).not.toContain('C:\\Users');

		// Native branch: Windows path for a Windows claude, /mnt path in the WSL fallback.
		const ps = buildClaudeWrappedCmd({ shellTarget: 'powershell', terminalId: 't1' }).at(-1) ?? '';
		expect(ps).toContain("& 'claude' '--dangerously-skip-permissions' '--settings' 'C:\\Users\\me");
		expect(ps).toContain('wsl.exe -e bash -l -i -c');
		expect(ps).toContain('/mnt/c/Users/me/AppData/Local/app.ikenga/claude-hooks-t1.json');
		__setClaudeSettingsPathForTests(null);
	});

	it('wraps Antigravity (agy) CLI correctly', () => {
		const cmd = buildAgentWrappedCmd({
			engine: 'antigravity',
			resumeSessionId: 'conv-123',
			model: 'gemini-pro',
			prompt: 'inspect workspace',
			shellTarget: 'bash',
		});
		expect(extractInvocation(cmd)).toBe(
			`'agy' '--conversation' 'conv-123' '--model' 'gemini-pro' 'inspect workspace'`
		);
		const script = cmd.at(-1) ?? '';
		expect(script).toContain('[antigravity exited');
	});

	it('wraps OpenAI Codex CLI correctly', () => {
		const cmd = buildAgentWrappedCmd({
			engine: 'codex',
			resumeSessionId: 'thread-456',
			model: 'o3-mini',
			prompt: 'generate tests',
			shellTarget: 'bash',
		});
		expect(extractInvocation(cmd)).toBe(
			`'codex' 'resume' 'thread-456' '--model' 'o3-mini' 'generate tests'`
		);
		const script = cmd.at(-1) ?? '';
		expect(script).toContain('[codex exited');
	});

	it('wraps Gemini CLI correctly', () => {
		const cmd = buildAgentWrappedCmd({
			engine: 'gemini',
			model: 'gemini-2.0-flash',
			prompt: 'summarize',
			shellTarget: 'bash',
		});
		expect(extractInvocation(cmd)).toBe(`'gemini' '--model' 'gemini-2.0-flash' 'summarize'`);
		const script = cmd.at(-1) ?? '';
		expect(script).toContain('[gemini exited');
	});

	it('starts a fresh interactive session with no flags by default for Claude', () => {
		const cmd = buildClaudeWrappedCmd({ shellTarget: 'bash' });
		expect(extractInvocation(cmd)).toBe(`'claude' '--dangerously-skip-permissions'`);
	});

	it('passes the prompt POSITIONALLY, not as -p (no headless print mode)', () => {
		const cmd = buildClaudeWrappedCmd({ prompt: '[via: groundwork/wp-card]', shellTarget: 'bash' });
		const run = extractInvocation(cmd);
		expect(run).not.toContain(`'-p'`);
		expect(run).not.toContain(`'--print'`);
		expect(run).toBe(`'claude' '--dangerously-skip-permissions' '[via: groundwork/wp-card]'`);
	});

	it('places the positional prompt last, after every flag', () => {
		const cmd = buildClaudeWrappedCmd({
			prompt: 'do the thing',
			permissionMode: 'plan',
			model: 'opus',
			resumeSessionId: 'abc-123',
			shellTarget: 'bash',
		});
		expect(extractInvocation(cmd)).toBe(
			`'claude' '--dangerously-skip-permissions' '--resume' 'abc-123' '--permission-mode' 'plan' '--model' 'opus' 'do the thing'`
		);
	});

	it('emits --resume when a session id is given', () => {
		const cmd = buildClaudeWrappedCmd({ resumeSessionId: 'sess-9', shellTarget: 'bash' });
		expect(extractInvocation(cmd)).toBe(
			`'claude' '--dangerously-skip-permissions' '--resume' 'sess-9'`
		);
	});

	it('shell-escapes prompts containing single quotes', () => {
		const cmd = buildClaudeWrappedCmd({ prompt: "it's fine", shellTarget: 'bash' });
		expect(extractInvocation(cmd)).toContain(`'it'\\''s fine'`);
	});

	it('emits --teammate-mode when teammateMode option is given', () => {
		const cmd = buildClaudeWrappedCmd({ teammateMode: 'in-process', shellTarget: 'bash' });
		expect(extractInvocation(cmd)).toBe(
			`'claude' '--dangerously-skip-permissions' '--teammate-mode' 'in-process'`
		);
	});

	it('ignores empty/nullish optional fields', () => {
		const cmd = buildClaudeWrappedCmd({
			prompt: '',
			resumeSessionId: null,
			permissionMode: null,
			model: undefined,
			shellTarget: 'bash',
			teammateMode: null,
		});
		expect(extractInvocation(cmd)).toBe(`'claude' '--dangerously-skip-permissions'`);
	});
});

// ── `--settings` injection (ikenga#149) ──────────────────────────────────────
//
// The shell's cost HUD / tool-call feed / permission inbox are fed by Claude
// Code hooks pointed at the iyke bridge. That wiring used to arrive via a
// CLAUDE_CONFIG_DIR overlay baked with `port: 0`, so it never worked. It now
// rides `claude --settings <file>`, whose path is derived from the LIVE
// endpoint. These assert the flag is present when primed, absent when not, and
// scoped to claude — `agy`/`codex`/`gemini` have no such protocol.
describe('claude --settings injection', () => {
	afterEach(() => {
		__setClaudeSettingsPathForTests(null);
	});

	it('passes --settings when the path has been primed', () => {
		__setClaudeSettingsPathForTests('/run/user/1000/app.ikenga/claude-hooks-settings.json');
		const cmd = buildClaudeWrappedCmd({ shellTarget: 'bash' });
		const script = cmd.join(' ');
		expect(script).toContain('--settings');
		expect(script).toContain('/run/user/1000/app.ikenga/claude-hooks-settings.json');
	});

	it('omits --settings entirely when unprimed', () => {
		__setClaudeSettingsPathForTests(null);
		const cmd = buildClaudeWrappedCmd({ shellTarget: 'bash' });
		expect(cmd.join(' ')).not.toContain('--settings');
	});

	it('does not leak --settings into non-claude engines', () => {
		__setClaudeSettingsPathForTests('/tmp/hooks.json');
		for (const engine of ['antigravity', 'codex', 'gemini'] as const) {
			const cmd = buildAgentWrappedCmd({ engine, shellTarget: 'bash' });
			expect(cmd.join(' ')).not.toContain('--settings');
		}
	});

	it('builds a per-terminal --settings path when a terminal id is supplied', () => {
		__setClaudeSettingsPathForTests('/run/user/1000/app.ikenga/claude-hooks-settings.json');
		const cmd = buildClaudeWrappedCmd({ shellTarget: 'bash', terminalId: 'term-xyz' });
		const script = cmd.join(' ');
		expect(script).toContain('--settings');
		expect(script).toContain('/run/user/1000/app.ikenga/claude-hooks-term-xyz.json');
		// And the legacy shared file is NOT used.
		expect(script).not.toContain('/run/user/1000/app.ikenga/claude-hooks-settings.json');
	});

	it('passes --resume before the positional prompt when resuming a claude session', () => {
		__setClaudeSettingsPathForTests('/run/user/1000/app.ikenga/claude-hooks-settings.json');
		const cmd = buildClaudeWrappedCmd({
			shellTarget: 'bash',
			terminalId: 'term-abc',
			resumeSessionId: 'sess-resume-123',
			prompt: 'continue where we left off',
		});
		expect(extractInvocation(cmd)).toBe(
			`'claude' '--dangerously-skip-permissions' '--settings' '/run/user/1000/app.ikenga/claude-hooks-term-abc.json' '--resume' 'sess-resume-123' 'continue where we left off'`
		);
	});
});

describe('WP-11 launch options (role, appendSystemPrompt, pluginDirs)', () => {
	const flag = (args: string[], name: string) => {
		const i = args.indexOf(name);
		return i === -1 ? undefined : args[i + 1];
	};

	it('adds nothing when none of the options is set', () => {
		const args = buildAgentArgs({ engine: 'claude' });
		expect(args).not.toContain('--model');
		expect(args).not.toContain('--append-system-prompt');
		expect(buildAgentEnv({ engine: 'claude' })).toBeUndefined();
	});

	it('resolves --model from the catalog by role', () => {
		expect(flag(buildAgentArgs({ role: 'chi' }), '--model')).toBe('claude-sonnet-5-5');
		expect(flag(buildAgentArgs({ role: 'pane' }), '--model')).toBe('claude-sonnet-5-5');
		expect(flag(buildAgentArgs({ role: 'plan' }), '--model')).toBe('claude-opus-5-5');
	});

	it('keeps an explicit model over the role default', () => {
		expect(flag(buildAgentArgs({ role: 'plan', model: 'claude-haiku-4-5' }), '--model')).toBe(
			'claude-haiku-4-5'
		);
	});

	it('passes --append-system-prompt before the positional prompt', () => {
		const args = buildAgentArgs({ appendSystemPrompt: 'You are in Ikenga.', prompt: 'hi' });
		expect(flag(args, '--append-system-prompt')).toBe('You are in Ikenga.');
		expect(args.at(-1)).toBe('hi');
	});

	it('sets CLAUDE_CODE_PLUGIN_DIRS for claude only', () => {
		expect(buildAgentEnv({ pluginDirs: ['/p/one', '', '/p/two'] })).toEqual({
			CLAUDE_CODE_PLUGIN_DIRS: '/p/one:/p/two',
		});
		expect(buildAgentEnv({ engine: 'codex', pluginDirs: ['/p'] })).toBeUndefined();
		expect(buildAgentEnv({ pluginDirs: [] })).toBeUndefined();
	});

	it('threads the plugin dirs into the PTY spawn env, keeping the tab env', () => {
		const tab = {
			id: 't1',
			title: 'claude',
			spec: { cwd: '/w', cmd: ['claude'], env: { FOO: '1' }, wrap: { pluginDirs: ['/p'] } },
		} as unknown as TerminalTab;
		expect(buildSpawnOpts(tab, 't1').env).toEqual({ FOO: '1', CLAUDE_CODE_PLUGIN_DIRS: '/p' });
		const plain = { ...tab, spec: { ...tab.spec, wrap: {} } } as unknown as TerminalTab;
		expect(buildSpawnOpts(plain, 't1').env).toEqual({ FOO: '1' });
	});
});
