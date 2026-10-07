// openLoginTerminal — "Run <engine> login" from the target picker (WP-3): a
// WSL-only CLI is detected as the display string `"/usr/bin/claude (WSL)"`,
// which used to be executed as if it were a path.

import { beforeEach, describe, expect, it, vi } from 'vitest';

const m = vi.hoisted(() => ({
	settingsGet: vi.fn(async (_key: string): Promise<string | null> => null),
	createTerminalSession: vi.fn((_opts: { cmd: string[]; title: string }) => 'term-1'),
	placeView: vi.fn(),
}));

vi.mock('@/lib/tauri-cmd', () => ({ settingsGet: m.settingsGet }));
vi.mock('@/lib/shell-profiles', () => ({ AGENT_WSL_DISTRO_KEY: 'terminal.agent_wsl_distro' }));
vi.mock('@/terminal/single-terminal', () => ({ createTerminalSession: m.createTerminalSession }));
vi.mock('@/lib/panes/pane-store', () => ({
	usePaneStore: { getState: () => ({ focusedId: 'p1', placeView: m.placeView }) },
}));

import { openLoginTerminal } from './login-terminal';

describe('openLoginTerminal', () => {
	beforeEach(() => {
		m.settingsGet.mockReset();
		m.createTerminalSession.mockClear();
		m.placeView.mockClear();
	});

	it('logs a WSL-only CLI in inside the configured distro', async () => {
		m.settingsGet.mockResolvedValue('Debian');
		await openLoginTerminal({ id: 'claude-code', executable_path: '/usr/bin/claude (WSL)' });
		expect(m.settingsGet).toHaveBeenCalledWith('terminal.agent_wsl_distro');
		const { cmd } = m.createTerminalSession.mock.calls[0][0];
		expect(cmd.slice(0, 3)).toEqual(['wsl.exe', '-d', 'Debian']);
		expect(cmd.at(-1)).toBe("'/usr/bin/claude' login");
		expect(m.placeView).toHaveBeenCalledWith('p1', { kind: 'terminal', sessionId: 'term-1' }, 'append');
	});

	it('falls back to the default distro when the setting cannot be read', async () => {
		m.settingsGet.mockRejectedValue(new Error('ipc down'));
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
		await openLoginTerminal({ id: 'claude-code', executable_path: '/usr/bin/claude (WSL)' });
		const { cmd } = m.createTerminalSession.mock.calls[0][0];
		expect(cmd.slice(0, 2)).toEqual(['wsl.exe', '-e']);
		expect(warn).toHaveBeenCalled();
		warn.mockRestore();
	});

	it('runs a host CLI directly without reading the distro', async () => {
		await openLoginTerminal({ id: 'codex', executable_path: 'C:\\npm\\codex.cmd' });
		expect(m.settingsGet).not.toHaveBeenCalled();
		expect(m.createTerminalSession.mock.calls[0][0].cmd).toEqual(['C:\\npm\\codex.cmd', 'login']);
	});
});
