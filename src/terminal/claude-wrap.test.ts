// Unit tests for buildAgentWrappedCmd & buildClaudeWrappedCmd — the argv builder behind every
// "open AI agent in a terminal" affordance.

import { describe, expect, it } from 'vitest';

import { buildAgentWrappedCmd, buildClaudeWrappedCmd } from './claude-wrap';

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
		expect(cmd).toContain('bash');
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
