// Gap audit rank 13 — a browser session must not auto-install pkg updates:
// the install path (`pkg_trust_preview_incoming`, `pkg_install_from_registry`)
// is not served by the headless daemon, so it only ever produced a persistent
// "N packages failed to update" banner. The desktop still auto-installs.

import { cleanup, render } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	remote: false,
	mutate: vi.fn(),
	updates: [{ id: 'com.example.a', name: 'A', latest: '2.0.0' }],
}));

vi.mock('@/lib/tauri-cmd', () => ({
	isRemoteWebSession: () => h.remote,
}));
vi.mock('@/lib/pkgs/use-derived', () => ({
	usePkgsDerived: () => ({ updates: h.updates }),
}));
vi.mock('@/lib/pkgs/use-update-pkgs', () => ({
	useUpdatePkgs: () => ({ isPending: false, mutate: h.mutate }),
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
	) => sel({ updatesAutoCheck: true, updatesAutoInstallPkgs: true }),
}));

import { PkgAutoUpdater } from './pkg-auto-updater';

describe('PkgAutoUpdater — remote session (gap rank 13)', () => {
	beforeEach(() => {
		h.mutate.mockReset();
	});
	afterEach(() => {
		cleanup();
		h.remote = false;
	});

	it('does not auto-install in a remote browser session', () => {
		h.remote = true;
		const { container } = render(<PkgAutoUpdater />);
		expect(h.mutate).not.toHaveBeenCalled();
		expect(container.textContent).not.toMatch(/failed to update/);
	});

	it('still auto-installs on the desktop', () => {
		h.remote = false;
		render(<PkgAutoUpdater />);
		expect(h.mutate).toHaveBeenCalledTimes(1);
		expect(h.mutate.mock.calls[0][0].rows).toEqual(h.updates);
	});
});
