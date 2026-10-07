// D-10 / D-18: an engine detection couldn't check (WSL couldn't be asked) is
// never "Not on PATH" and can't be picked. The step shows ONE
// "WSL unavailable — <reason>" notice above the grid; each affected card shows
// only a short dimmed "WSL unavailable" chip.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

// Only the card renders; stub the step's heavier collaborators so the test
// doesn't pull the whole shell (store, registry, IPC) in.
vi.mock('@/lib/tauri-cmd', () => ({
	detectAgent: vi.fn(),
	isRemoteWebSession: () => false,
	pkgInstallFromRegistry: vi.fn(),
	pkgKernelStatus: vi.fn(),
}));
vi.mock('@/lib/registry/client', () => ({
	fetchIndex: vi.fn(),
	fetchPkgDetail: vi.fn(),
	resolveInstallPlan: vi.fn(),
}));
vi.mock('@/lib/settings/client', () => ({ openSettingsFile: vi.fn() }));
vi.mock('@/lib/shell/shell-store', () => {
	const state = {
		onboarding: { selectedAgentId: null },
		setSelectedAgentId: () => {},
		setDefaultEngineId: () => {},
	};
	return { useShellStore: (sel: (s: typeof state) => unknown) => sel(state) };
});
vi.mock('@/shell/onboarding/footer', () => ({ WritesNote: () => null }));
vi.mock('./use-onboarding-step', () => ({
	useOnboardingStep: () => ({ record: {}, setPayload: () => {} }),
}));
vi.mock('@/shell/onboarding/engine-logo', () => ({ EngineLogo: () => null }));
vi.mock('@/lib/transport', () => ({ openExternalUrl: vi.fn() }));

import { type AgentDetectMap, entryFromProbe } from '@/lib/shell/use-agent-detect';
import type { DetectedAgent } from '@/lib/tauri-cmd';

import { EngineBody, EngineCard, SUPPORTED_ENGINES } from './engine-body';

afterEach(cleanup);

const claude = SUPPORTED_ENGINES.find((e) => e.id === 'claude-code') ?? SUPPORTED_ENGINES[0];

const unavailable: DetectedAgent = {
	id: claude.id,
	display: claude.display,
	executable_path: 'claude (WSL)',
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
	unavailable: { kind: 'wsl', reason: 'Wsl/Service/CreateInstance/E_FAIL' },
};

describe('EngineCard when WSL could not be asked', () => {
	it('shows only a short WSL unavailable chip, not "Not on PATH", and is not selectable', () => {
		const onSelect = vi.fn();
		render(
			<EngineCard
				meta={claude}
				entry={entryFromProbe(unavailable)}
				selected={false}
				onSelect={onSelect}
				onOpenDocs={() => {}}
			/>
		);
		const pill = screen.getByTestId('status-pill');
		expect(pill.getAttribute('data-status')).toBe('unavailable');
		expect(pill.textContent).toBe('WSL unavailable');
		// D-18: the reason lives in the step's one notice, not on the card.
		const card0 = screen.getByTestId('agent-card');
		expect(card0.textContent).not.toMatch(/E_FAIL/);
		expect(screen.queryByText(/Not on PATH/i)).toBeNull();
		expect(screen.queryByTestId('agent-install-cmd')).toBeNull();

		const card = screen.getByTestId('agent-card');
		expect(card.getAttribute('aria-disabled')).toBe('true');
		fireEvent.click(card);
		expect(onSelect).not.toHaveBeenCalled();
	});
});

describe('EngineBody when WSL could not be asked (D-18)', () => {
	function renderBody(results: AgentDetectMap) {
		const qc = new QueryClient();
		return render(
			<QueryClientProvider client={qc}>
				<EngineBody onContinue={() => {}} results={results} refresh={() => {}} />
			</QueryClientProvider>
		);
	}

	const codex = SUPPORTED_ENGINES.find((e) => e.id !== claude.id) ?? SUPPORTED_ENGINES[1];

	it('shows one notice above the grid with the first affected reason', () => {
		renderBody({
			[claude.id]: entryFromProbe(unavailable),
			[codex.id]: entryFromProbe({
				...unavailable,
				id: codex.id,
				display: codex.display,
				unavailable: { kind: 'wsl', reason: 'timed out' },
			}),
		});
		const notices = screen.getAllByTestId('wsl-unavailable-notice');
		expect(notices).toHaveLength(1);
		const first =
			SUPPORTED_ENGINES.findIndex((e) => e.id === claude.id) <
			SUPPORTED_ENGINES.findIndex((e) => e.id === codex.id)
				? 'Wsl/Service/CreateInstance/E_FAIL'
				: 'timed out';
		expect(within(notices[0]).getByTestId('wsl-unavailable-notice-text').textContent).toBe(
			`WSL unavailable — ${first}`
		);
		// The notice sits above the grid.
		const grid = screen.getByTestId('agents-grid');
		expect(
			notices[0].compareDocumentPosition(grid) & Node.DOCUMENT_POSITION_FOLLOWING
		).toBeTruthy();
		// Each affected card: chip only, no reason.
		const cards = within(grid)
			.getAllByTestId('agent-card')
			.filter((c) => c.getAttribute('data-status') === 'unavailable');
		expect(cards).toHaveLength(2);
		for (const c of cards) {
			expect(within(c).getByTestId('status-pill').textContent).toBe('WSL unavailable');
			expect(c.textContent).not.toMatch(/E_FAIL|timed out/);
		}
	});

	it('shows no notice when no engine is WSL unavailable', () => {
		renderBody({ [claude.id]: { status: 'missing' } });
		expect(screen.queryByTestId('wsl-unavailable-notice')).toBeNull();
	});
});
