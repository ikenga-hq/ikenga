/**
 * Build the argv vector that `Pty.spawn` / `createTerminalSession` expects for
 * "open AI agent in a terminal" affordances. Wraps agent CLI invocations
 * (`claude`, `agy`, `codex`, `gemini`) in a shell script that runs them once,
 * prints the exit code on non-zero, then drops the user back into an interactive
 * shell so the PTY survives any failure mode (stale resume id, tool error, success).
 *
 * Used by: session-detail "Open in terminal", new-session dialog Terminal
 * mode, /claude route, and New Tab / Dock terminal affordances.
 */

import { isWindows } from '@/lib/platform';

export type AgentEngineKind = 'claude' | 'antigravity' | 'codex' | 'gemini';

export interface AgentWrapOpts {
	/** AI engine to launch. Defaults to 'claude'. */
	engine?: AgentEngineKind;
	/** Session/conversation id to resume. Omit to start a fresh session. */
	resumeSessionId?: string | null;
	/** Initial prompt — passed as a positional arg so the agent starts an
	 *  interactive session seeded with it. */
	prompt?: string | null;
	/** Permission mode (e.g. `default` | `acceptEdits` | `plan` for Claude). */
	permissionMode?: string | null;
	model?: string | null;
	/** Target execution environment. 'native' | 'wsl' | 'bash' | 'posix' | 'powershell' | 'pwsh'. */
	shellTarget?: 'native' | 'wsl' | 'bash' | 'posix' | 'powershell' | 'pwsh';
	/** Target WSL distribution name (e.g. 'Ubuntu', 'Debian') when shellTarget === 'wsl'. */
	wslDistro?: string | null;
	/** Working directory for WSL launch. */
	/** `in-process` | etc. — becomes `--teammate-mode`. (G-08) */
	teammateMode?: string | null;
}

export type ClaudeWrapOpts = AgentWrapOpts;

/** POSIX single-quote escape: wrap in `'…'`, replace each `'` with `'\''`. */
function shQuote(arg: string): string {
	return `'${arg.replace(/'/g, `'\\''`)}'`;
}

/** PowerShell single-quote escape: wrap in `'…'`, double each interior `'`. */
function psQuote(arg: string): string {
	return `'${arg.replace(/'/g, `''`)}'`;
}

export function buildAgentArgs(opts: AgentWrapOpts): string[] {
	const engine = opts.engine ?? 'claude';
	switch (engine) {
		case 'antigravity': {
			const args = ['agy'];
			if (opts.resumeSessionId) args.push('--conversation', opts.resumeSessionId);
			if (opts.model) args.push('--model', opts.model);
			if (opts.prompt) args.push(opts.prompt);
			return args;
		}
		case 'codex': {
			const args = ['codex'];
			if (opts.resumeSessionId) args.push('resume', opts.resumeSessionId);
			if (opts.model) args.push('--model', opts.model);
			if (opts.prompt) args.push(opts.prompt);
			return args;
		}
		case 'gemini': {
			const args = ['gemini'];
			if (opts.model) args.push('--model', opts.model);
			if (opts.prompt) args.push(opts.prompt);
			return args;
		}
		case 'claude':
		default: {
			const args = ['claude', '--dangerously-skip-permissions'];
			if (opts.resumeSessionId) args.push('--resume', opts.resumeSessionId);
			if (opts.permissionMode) args.push('--permission-mode', opts.permissionMode);
			if (opts.model) args.push('--model', opts.model);
			if (opts.teammateMode) args.push('--teammate-mode', opts.teammateMode);
			// Positional, last — seeds an interactive session. `-p` would force
			// headless print mode; see AgentWrapOpts.prompt.
			if (opts.prompt) args.push(opts.prompt);
			return args;
		}
	}
}

export function buildAgentWrappedCmd(opts: AgentWrapOpts = {}): string[] {
	const engine = opts.engine ?? 'claude';
	const args = buildAgentArgs(opts);
	const target = opts.shellTarget ?? (isWindows ? 'native' : 'posix');

	if (target === 'wsl') {
		const quoted = args.map(shQuote).join(' ');
		const script =
			`printf '\\033[2m$ %s\\033[0m\\n' ${shQuote(quoted)}; ` +
			`${quoted}; ` +
			`__status=$?; ` +
			`if [ $__status -ne 0 ]; then printf '\\n\\033[31m[${engine} exited %d]\\033[0m\\n' $__status; fi; ` +
			`exec "\${SHELL:-bash}" -i`;
		const wslCmd = ['wsl.exe'];
		if (opts.wslDistro) {
			wslCmd.push('-d', opts.wslDistro);
		}
		if (opts.cwd) {
			// Convert Windows path separators to forward slashes for WSL compatibility
			wslCmd.push('--cd', opts.cwd.replace(/\\/g, '/'));
		}
		wslCmd.push('bash', '-l', '-i', '-c', script);
		return wslCmd;
	}

	if (target === 'bash' || target === 'posix') {
		const quoted = args.map(shQuote).join(' ');
		const script =
			`printf '\\033[2m$ %s\\033[0m\\n' ${shQuote(quoted)}; ` +
			`${quoted}; ` +
			`__status=$?; ` +
			`if [ $__status -ne 0 ]; then printf '\\n\\033[31m[${engine} exited %d]\\033[0m\\n' $__status; fi; ` +
			`exec "\${SHELL:-bash}" -i`;
		return ['/bin/bash', '-i', '-c', script];
	}

	if (isWindows || target === 'powershell' || target === 'pwsh' || target === 'native') {
		const quoted = args.map(psQuote).join(' ');
		const binName = args[0];
		const subArgs = args.slice(1).map(psQuote).join(' ');
		const psExe = target === 'pwsh' ? 'pwsh.exe' : 'powershell.exe';
		const wslScript = `${args.map(shQuote).join(' ')}; __status=$?; if [ $__status -ne 0 ]; then printf '\\n\\033[31m[${engine} exited %d]\\033[0m\\n' $__status; fi`;
		const script =
			`Write-Host ('$ ' + ${psQuote(quoted)}) -ForegroundColor DarkGray; ` +
			`if (Get-Command ${psQuote(binName)} -ErrorAction SilentlyContinue) { & ${psQuote(binName)} ${subArgs} } ` +
			`elseif (Get-Command 'wsl.exe' -ErrorAction SilentlyContinue) { wsl.exe bash -l -i -c ${psQuote(wslScript)} } ` +
			`else { & ${psQuote(binName)} ${subArgs} }; ` +
			`$code = $LASTEXITCODE; ` +
			`if ($code -ne 0) { Write-Host ('[${engine} exited ' + $code + ']') -ForegroundColor Red }; ` +
			`if (Get-Command pwsh -ErrorAction SilentlyContinue) { pwsh -NoLogo } else { powershell -NoLogo }`;
		return [psExe, '-NoLogo', '-ExecutionPolicy', 'Bypass', '-Command', script];
	}

	const quoted = args.map(shQuote).join(' ');
	const script =
		`printf '\\033[2m$ %s\\033[0m\\n' ${shQuote(quoted)}; ` +
		`${quoted}; ` +
		`__status=$?; ` +
		`if [ $__status -ne 0 ]; then printf '\\n\\033[31m[${engine} exited %d]\\033[0m\\n' $__status; fi; ` +
		`exec "\${SHELL:-bash}" -i`;
	return ['/bin/bash', '-i', '-c', script];
}

/** Legacy alias for buildAgentWrappedCmd with engine='claude' */
export function buildClaudeWrappedCmd(opts: ClaudeWrapOpts = {}): string[] {
	return buildAgentWrappedCmd({ ...opts, engine: opts.engine ?? 'claude' });
}
