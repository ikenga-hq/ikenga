// Unit tests for buildClaudeWrappedCmd — the argv builder behind every
// "open claude in a terminal" affordance (open-session-dialog Terminal mode,
// /claude Run-command, new-session dialog, session resume, composer action).
//
// The load-bearing invariant: the prompt is passed POSITIONALLY, never as
// `-p`. `-p` forces claude into headless print mode (one-shot, no TTY), which
// is wrong for a terminal the user is meant to keep driving. Regression guard
// for the `[via: groundwork/wp-card] -p` bug.

import { describe, expect, it } from 'vitest';

import { buildClaudeWrappedCmd } from './claude-wrap';

/** Pull the quoted `claude …` invocation out of the bash wrapper script so
 *  assertions read against the real command rather than the printf chrome. */
function claudeInvocation(cmd: string[]): string {
	const script = cmd.at(-1) ?? '';
	// The wrapper is `printf '…' '<quoted>'; <quoted>; __status=$?; …`.
	// The second occurrence (after the printf) is the actual run.
	const runStart = script.indexOf('; ') + 2;
	const runEnd = script.indexOf('; __status=$?');
	return script.slice(runStart, runEnd);
}

describe('buildClaudeWrappedCmd', () => {
	it('wraps the invocation in an interactive bash script that survives exit', () => {
		const cmd = buildClaudeWrappedCmd();
		expect(cmd.slice(0, 3)).toEqual(['/bin/bash', '-i', '-c']);
		const script = cmd.at(-1) ?? '';
		expect(script).toContain('exec "$SHELL" -i');
		expect(script).toContain('[claude exited');
	});

	it('starts a fresh interactive session with no flags by default', () => {
		const cmd = buildClaudeWrappedCmd();
		expect(claudeInvocation(cmd)).toBe(`'claude' '--dangerously-skip-permissions'`);
	});

	it('passes the prompt POSITIONALLY, not as -p (no headless print mode)', () => {
		const cmd = buildClaudeWrappedCmd({ prompt: '[via: groundwork/wp-card]' });
		const run = claudeInvocation(cmd);
		// Each arg is shell-quoted, so a real flag appears as a standalone
		// `'-p'` / `'--print'` token — distinct from the `-p` substring inside
		// `'--dangerously-skip-permissions'`.
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
		});
		expect(claudeInvocation(cmd)).toBe(
			`'claude' '--dangerously-skip-permissions' '--resume' 'abc-123' '--permission-mode' 'plan' '--model' 'opus' 'do the thing'`
		);
	});

	it('emits --resume when a session id is given', () => {
		const cmd = buildClaudeWrappedCmd({ resumeSessionId: 'sess-9' });
		expect(claudeInvocation(cmd)).toBe(
			`'claude' '--dangerously-skip-permissions' '--resume' 'sess-9'`
		);
	});

	it('shell-escapes prompts containing single quotes', () => {
		const cmd = buildClaudeWrappedCmd({ prompt: "it's fine" });
		// POSIX `'…'` escape: close, escaped quote, reopen.
		expect(claudeInvocation(cmd)).toContain(`'it'\\''s fine'`);
	});

	it('emits --teammate-mode when teammateMode option is given', () => {
		const cmd = buildClaudeWrappedCmd({ teammateMode: 'in-process' });
		expect(claudeInvocation(cmd)).toBe(
			`'claude' '--dangerously-skip-permissions' '--teammate-mode' 'in-process'`
		);
	});

	it('ignores empty/nullish optional fields', () => {
		const cmd = buildClaudeWrappedCmd({
			prompt: '',
			resumeSessionId: null,
			permissionMode: null,
			model: undefined,
			teammateMode: null,
		});
		expect(claudeInvocation(cmd)).toBe(`'claude' '--dangerously-skip-permissions'`);
	});
});
