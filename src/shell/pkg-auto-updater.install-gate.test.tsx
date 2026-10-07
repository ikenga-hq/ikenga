// Gap audit rank 3 — the manual "Update all" banner button must not offer an
// install the daemon cannot run: in a browser session it is disabled and reads
// "Not available on this server yet". The desktop keeps the working button.

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	remote: false,
	updates: [{ id: 'com.example.a', name: 'A', latest: '2.0.0' }],
}));

vi.mock('@/lib/tauri-cmd', () => ({
	isRemoteWebSession: () => h.remote,
}));
vi.mock('@/lib/pkgs/use-derived', () => ({
	usePkgsDerived: () => ({ updates: h.updates }),
}));
vi.mock('@/lib/pkgs/use-update-pkgs', () => ({
	useUpdatePkgs: () => ({ isPending: false, mutate: vi.fn() }),
}));
vi.mock('@/lib/notifications/record-update', () => ({
	recordPkgUpdatesAvailable: vi.fn(async () => {}),
}));
vi.mock('@/lib/updater/sheet-store', () => ({
	useUpdateSheetStore: (sel: (s: { openSheet: () => void }) => unknown) =>
		sel({ openSheet: () => {} }),
}));
vi.mock('@/lib/shell/shell-store', () => ({
	useShellStore: (
		sel: (s: { updatesAutoCheck: boolean; updatesAutoInstallPkgs: boolean }) => unknown
	) => sel({ updatesAutoCheck: true, updatesAutoInstallPkgs: false }),
}));

import { PkgAutoUpdater } from './pkg-auto-updater';

afterEach(() => {
	cleanup();
	h.remote = false;
});

describe('PkgAutoUpdater manual banner — install gate (gap rank 3)', () => {
	it('disables "Update all" with the honest reason in a remote session', () => {
		h.remote = true;
		render(<PkgAutoUpdater />);
		const btn = screen.getByRole('button', { name: 'Not available on this server yet' });
		expect((btn as HTMLButtonElement).disabled).toBe(true);
		expect(screen.queryByText(/Update all/)).toBeNull();
	});

	it('keeps "Update all (N)" working on the desktop', () => {
		render(<PkgAutoUpdater />);
		const btn = screen.getByRole('button', { name: 'Update all (1)' });
		expect((btn as HTMLButtonElement).disabled).toBe(false);
	});
});
