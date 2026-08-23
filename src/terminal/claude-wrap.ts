/**
 * Build the argv vector that `Pty.spawn` / `createTerminalSession` expects for
 * "open Claude in a terminal" affordances. Wraps `claude <flags>` in a shell
 * script that runs it once, prints the exit code on non-zero, then drops the
 * user back into an interactive shell so the PTY survives any failure mode
 * (stale --resume id, killed-mid-tool JSONL, success). The user sees claude's
 * real stderr and can edit/retry the command in a live shell.
 *
 * Used by: session-detail "Open in terminal", new-session dialog Terminal
 * mode, and the /claude route's "Run command" buttons.
 */

import { isWindows } from '@/lib/platform';

export interface ClaudeWrapOpts {
	/** Claude session id to resume. Omit to start a fresh session. */
	resumeSessionId?: string | null;
	/** Initial prompt — passed as a positional arg so claude starts an
	 *  interactive session seeded with it. Not `-p`: that flag runs claude
	 *  in headless print mode (one-shot, no TTY), which is wrong for a
	 *  terminal spawn the user is meant to keep driving. */
	prompt?: string | null;
	/** `default` | `acceptEdits` | `plan` | etc. — becomes `--permission-mode`. */
	permissionMode?: string | null;
	model?: string | null;
	/** `in-process` | etc. — becomes `--teammate-mode`. (G-08) */
	teammateMode?: string | null;
}

/** POSIX single-quote escape: wrap in `'…'`, replace each `'` with `'\''`. */
function shQuote(arg: string): string {
	return `'${arg.replace(/'/g, `'\\''`)}'`;
}

/** PowerShell single-quote escape: wrap in `'…'`, double each interior `'`. */
function psQuote(arg: string): string {
	return `'${arg.replace(/'/g, `''`)}'`;
}

function claudeArgs(opts: ClaudeWrapOpts): string[] {
	const args = ['claude', '--dangerously-skip-permissions'];
	if (opts.resumeSessionId) args.push('--resume', opts.resumeSessionId);
	if (opts.permissionMode) args.push('--permission-mode', opts.permissionMode);
	if (opts.model) args.push('--model', opts.model);
	if (opts.teammateMode) args.push('--teammate-mode', opts.teammateMode);
	// Positional, last — seeds an interactive session. `-p` would force
	// headless print mode; see ClaudeWrapOpts.prompt.
	if (opts.prompt) args.push(opts.prompt);
	return args;
}

export function buildClaudeWrappedCmd(opts: ClaudeWrapOpts = {}): string[] {
	const args = claudeArgs(opts);

	if (isWindows) {
		// Windows: shell out via PowerShell. `bash.exe` on Windows is the WSL
		// launcher and breaks loudly if WSL isn't configured (the failure mode
		// users have hit), so never reach for bash here.
		const quoted = args.map(psQuote).join(' ');
		const invocation = `& ${args.map(psQuote).join(' ')}`;
		const script =
			`Write-Host ('$ ' + ${psQuote(quoted)}) -ForegroundColor DarkGray; ` +
			`${invocation}; ` +
			`$code = $LASTEXITCODE; ` +
			`if ($code -ne 0) { Write-Host ('[claude exited ' + $code + ']') -ForegroundColor Red }; ` +
			`powershell.exe -NoLogo`;
		return ['powershell.exe', '-NoLogo', '-NoProfile', '-Command', script];
	}

	const quoted = args.map(shQuote).join(' ');
	const script =
		`printf '\\033[2m$ %s\\033[0m\\n' ${shQuote(quoted)}; ` +
		`${quoted}; ` +
		`__status=$?; ` +
		`if [ $__status -ne 0 ]; then printf '\\n\\033[31m[claude exited %d]\\033[0m\\n' $__status; fi; ` +
		`exec "$SHELL" -i`;
	return ['/bin/bash', '-i', '-c', script];
}
