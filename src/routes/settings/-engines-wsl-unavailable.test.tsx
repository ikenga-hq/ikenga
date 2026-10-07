// D-18: Settings › Engines shows ONE "WSL unavailable — <reason>" notice above
// the agent rows when WSL couldn't be asked; the selected agent's row shows only
// a short dimmed "WSL unavailable" chip, not the reason again.

import type * as React from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { DetectedAgent } from '@/lib/tauri-cmd';

const detectAgents = vi.fn<() => Promise<DetectedAgent[]>>();

vi.mock('@/lib/tauri-cmd', () => ({
	detectAgents: () => detectAgents(),
	settingsGet: vi.fn(),
	settingsSet: vi.fn(),
}));
vi.mock('@tanstack/react-router', async (orig) => ({
	...(await orig<typeof import('@tanstack/react-router')>()),
	useNavigate: () => vi.fn(),
}));
vi.mock('@/lib/shell/shell-store', () => {
	const state = {
		onboarding: {
			selectedAgentId: 'claude-code',
			steps: { engine: { payload: { agentId: 'claude-code', authed: true } } },
		},
		defaultEngineId: 'claude-code',
		enterOnboardingEdit: () => {},
	};
	return { useShellStore: (sel: (s: typeof state) => unknown) => sel(state) };
});
vi.mock('@/shell/settings/field', () => ({
	SettingsFieldRow: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
	useSettingsSection: () => ({}),
}));
vi.mock('@/lib/shell-profiles', () => ({}));
vi.mock('@/lib/settings/client', () => ({ writeSettingsField: vi.fn() }));
vi.mock('@/terminal/single-terminal', () => ({}));
vi.mock('@/terminal/claude-wrap', () => ({ buildAgentWrappedCmd: vi.fn() }));
vi.mock('@/lib/transport', () => ({
	openExternalUrl: vi.fn(),
	listen: vi.fn(() => Promise.resolve(() => {})),
}));
vi.mock('@/lib/panes/pane-store', () => ({ usePaneStore: vi.fn() }));

import { EngineSectionBody } from './engines';

afterEach(cleanup);

function agent(id: string, display: string, over: Partial<DetectedAgent> = {}): DetectedAgent {
	return {
		id,
		display,
		executable_path: `${id} (WSL)`,
		version: null,
		authed: null,
		auth_hint: null,
		capabilities: {
			streaming: true,
			tool_use: true,
			thinking: false,
			artifacts: false,
			mcp: false,
			session_resume: false,
		},
		...over,
	};
}

function renderSection() {
	const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return render(
		<QueryClientProvider client={qc}>
			<EngineSectionBody />
		</QueryClientProvider>
	);
}

describe('Settings › Engines when WSL is unavailable (D-18)', () => {
	it('shows one notice with the first reason and a short chip on the row', async () => {
		detectAgents.mockResolvedValue([
			agent('claude-code', 'Claude Code', { unavailable: { kind: 'wsl', reason: 'E_FAIL' } }),
			agent('codex', 'Codex', { unavailable: { kind: 'wsl', reason: 'timed out' } }),
		]);
		renderSection();

		const notices = await screen.findAllByTestId('wsl-unavailable-notice');
		expect(notices).toHaveLength(1);
		expect(within(notices[0]).getByTestId('wsl-unavailable-notice-text').textContent).toBe(
			'WSL unavailable — E_FAIL'
		);
		expect(notices[0].textContent).toMatch(/wsl\.exe/);

		const chip = screen.getByTestId('engine-auth-unavailable');
		expect(chip.textContent).toBe('WSL unavailable');
		// The reason is in the notice only — not repeated in the page's text.
		expect(document.body.textContent?.match(/E_FAIL/g)).toHaveLength(1);
		expect(document.body.textContent).not.toMatch(/timed out/);
	});

	it('shows no notice when every agent was checked', async () => {
		detectAgents.mockResolvedValue([
			agent('claude-code', 'Claude Code', { executable_path: '/usr/bin/claude', authed: true }),
		]);
		renderSection();
		await screen.findByText('Signed in');
		expect(screen.queryByTestId('wsl-unavailable-notice')).toBeNull();
		expect(screen.queryByTestId('engine-auth-unavailable')).toBeNull();
	});

	it('shows the notice even when only a non-selected agent is affected', async () => {
		detectAgents.mockResolvedValue([
			agent('claude-code', 'Claude Code', { executable_path: '/usr/bin/claude', authed: true }),
			agent('codex', 'Codex', { unavailable: { kind: 'wsl', reason: 'timed out' } }),
		]);
		renderSection();
		const notice = await screen.findByTestId('wsl-unavailable-notice-text');
		expect(notice.textContent).toBe('WSL unavailable — timed out');
		expect(screen.queryByTestId('engine-auth-unavailable')).toBeNull();
	});
});
