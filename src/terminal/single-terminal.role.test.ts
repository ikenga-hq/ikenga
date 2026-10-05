// WP-11: a Claude terminal pane carries a launch role, so with no model it
// starts on the catalog model for that role, and an explicit model wins.

import { describe, expect, it, vi } from 'vitest';

const added: Array<{ spec: { cmd: string[]; wrap?: Record<string, unknown> }; id?: string }> = [];
vi.mock('./session-store', () => ({
	makeTerminalId: () => 'term-1',
	openTabPty: vi.fn(),
	useTerminalStore: {
		subscribe: () => () => {},
		getState: () => ({
			add: (
				spec: { cmd: string[]; wrap?: Record<string, unknown> },
				_title: string,
				id?: string
			) => {
				added.push({ spec, id });
				return id ?? 'term-1';
			},
		}),
	},
}));
vi.mock('./pty-registry', () => ({ getPty: () => undefined }));
vi.mock('./xterm-host', () => ({ XTermHost: () => null }));
vi.mock('@/lib/shell/active-project-cwd', () => ({ activeProjectCwd: () => '/work' }));

import { buildAgentArgs } from './claude-wrap';
import { createClaudeTerminalSession } from './single-terminal';

function modelFlags(args: string[]): string[] {
	return args.flatMap((a, i) => (a === '--model' ? [args[i + 1]] : []));
}

function lastWrapArgs(): string[] {
	const wrap = added.at(-1)?.spec.wrap ?? {};
	return buildAgentArgs(wrap);
}

describe('createClaudeTerminalSession role wiring', () => {
	it('defaults a Claude terminal to the pane role (catalog pane model)', () => {
		createClaudeTerminalSession();
		expect(added.at(-1)?.spec.wrap?.role).toBe('pane');
		expect(modelFlags(lastWrapArgs())).toEqual(['claude-sonnet-5-5']);
		const script = added.at(-1)?.spec.cmd.join(' ') ?? '';
		expect(script).toContain("'--model' 'claude-sonnet-5-5'");
	});

	it('launches a plan terminal on the plan model', () => {
		createClaudeTerminalSession({ role: 'plan' }, 'claude · plan');
		expect(modelFlags(lastWrapArgs())).toEqual(['claude-opus-5-5']);
	});

	it('keeps an explicit model over the role default, with one --model', () => {
		createClaudeTerminalSession({ role: 'plan', model: 'claude-haiku-4-5' });
		expect(modelFlags(lastWrapArgs())).toEqual(['claude-haiku-4-5']);
		const script = added.at(-1)?.spec.cmd.join(' ') ?? '';
		// The bash wrapper echoes the invocation before running it, so each
		// flag appears twice in the script; the role default never does.
		expect(script.match(/'--model'/g)).toHaveLength(2);
		expect(script).not.toContain('claude-opus-5-5');
	});
});
