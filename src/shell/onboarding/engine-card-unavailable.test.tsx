// D-10: an engine detection couldn't check (WSL couldn't be asked) shows
// "WSL unavailable — <reason>" on its onboarding card — never "Not on PATH" —
// and can't be picked.

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

// Only the card renders; stub the step's heavier collaborators so the test
// doesn't pull the whole shell (store, registry, IPC) in.
vi.mock('@/lib/tauri-cmd', () => ({
	detectAgent: vi.fn(),
	pkgInstallFromRegistry: vi.fn(),
	pkgKernelStatus: vi.fn(),
}));
vi.mock('@/lib/registry/client', () => ({
	fetchIndex: vi.fn(),
	fetchPkgDetail: vi.fn(),
	resolveInstallPlan: vi.fn(),
}));
vi.mock('@/lib/settings/client', () => ({ openSettingsFile: vi.fn() }));
vi.mock('@/lib/shell/shell-store', () => ({ useShellStore: vi.fn() }));
vi.mock('@/shell/onboarding/footer', () => ({ WritesNote: () => null }));
vi.mock('./use-onboarding-step', () => ({ useOnboardingStep: vi.fn() }));
vi.mock('@/shell/onboarding/engine-logo', () => ({ EngineLogo: () => null }));
vi.mock('@/lib/transport', () => ({ openExternalUrl: vi.fn() }));

import { entryFromProbe } from '@/lib/shell/use-agent-detect';
import type { DetectedAgent } from '@/lib/tauri-cmd';

import { EngineCard, SUPPORTED_ENGINES } from './engine-body';

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
	it('says WSL unavailable with the reason, not "Not on PATH", and is not selectable', () => {
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
		expect(pill.textContent).toContain('WSL unavailable');
		expect(screen.getByTestId('agent-unavailable').textContent).toContain(
			'WSL unavailable — Wsl/Service/CreateInstance/E_FAIL'
		);
		expect(screen.queryByText(/Not on PATH/i)).toBeNull();
		expect(screen.queryByTestId('agent-install-cmd')).toBeNull();

		const card = screen.getByTestId('agent-card');
		expect(card.getAttribute('aria-disabled')).toBe('true');
		fireEvent.click(card);
		expect(onSelect).not.toHaveBeenCalled();
	});
});
