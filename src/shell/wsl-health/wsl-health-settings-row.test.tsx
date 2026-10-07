// honest-failure-states WP-2 (D-7) — the Settings › Engines WSL health row
// shows what a launch / errno probe measured and probes only on "Check now":
// opening Settings is not a WSL launch, and a probe can cold-start the distro.

import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactNode } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
	probeWslHealth: vi.fn(() => Promise.resolve()),
	useWslHealth: vi.fn((_distro: string, _opts?: { enabled?: boolean }) => ({
		data: undefined,
		error: null,
	})),
}));

vi.mock('@/lib/wsl-health/fix-flow', () => ({ requestWslFix: vi.fn() }));
// The real row needs the settings shell's section context; only its body is
// under test here.
vi.mock('@/shell/settings/field', () => ({
	SettingsFieldRow: ({ children }: { children: ReactNode }) => <div>{children}</div>,
}));
vi.mock('@/lib/wsl-health/query', () => ({
	probeWslHealth: mocks.probeWslHealth,
	useWslHealth: mocks.useWslHealth,
}));

import { WslHealthSettingsRow } from './wsl-health-settings-row';

afterEach(cleanup);

describe('WslHealthSettingsRow', () => {
	it('never probes on mount; "Check now" forces one', async () => {
		render(<WslHealthSettingsRow distro="Ubuntu" />);
		expect(mocks.useWslHealth).toHaveBeenCalledWith('Ubuntu', { enabled: false });
		expect(screen.getByText('Not checked yet')).toBeTruthy();
		expect(mocks.probeWslHealth).not.toHaveBeenCalled();

		await userEvent.click(screen.getByRole('button', { name: /Check now/ }));
		expect(mocks.probeWslHealth).toHaveBeenCalledWith('Ubuntu', { force: true });
	});
});
