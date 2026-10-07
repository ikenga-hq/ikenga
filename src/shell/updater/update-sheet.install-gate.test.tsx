// Gap audit rank 3 — the update sheet's "Update all" (Apps and extensions tab)
// must not offer an install the daemon cannot run: in a browser session it is
// disabled and reads "Not available on this server yet".

import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ remote: false }));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	isRemoteWebSession: () => h.remote,
}));
vi.mock('@/lib/pkgs/use-derived', () => ({
	usePkgsDerived: () => ({
		updates: [{ id: 'com.example.a', name: 'A', version: '1.0.0', latest: '2.0.0' }],
		isLoading: false,
	}),
}));
vi.mock('@/lib/pkgs/use-update-pkgs', () => ({
	useUpdatePkgs: () => ({ isPending: false, mutate: vi.fn(), mutateAsync: vi.fn() }),
}));
vi.mock('@/lib/updater/use-updater', () => ({
	useUpdater: () => ({ available: null, installing: false, installed: false }),
}));
vi.mock('@/lib/updater/use-github-releases', () => ({
	useGitHubReleases: () => ({ data: [] }),
	findReleaseByVersion: () => undefined,
}));
vi.mock('@/lib/updater/restart-sessions', () => ({ useLiveSessionCount: () => 0 }));
vi.mock('@/components/pkg/trust-review-modal', () => ({ TrustReviewModal: () => null }));

import { useUpdateSheetStore } from '@/lib/updater/sheet-store';

import { UpdateSheet } from './update-sheet';

afterEach(() => {
	cleanup();
	h.remote = false;
	useUpdateSheetStore.getState().close();
});

function openPkgs() {
	render(<UpdateSheet />);
	act(() => useUpdateSheetStore.getState().openSheet('pkgs'));
}

describe('UpdateSheet "Update all" — install gate (gap rank 3)', () => {
	it('is disabled and reads the honest reason in a remote session', async () => {
		h.remote = true;
		openPkgs();
		const btn = await screen.findByRole('button', { name: 'Not available on this server yet' });
		expect((btn as HTMLButtonElement).disabled).toBe(true);
		expect(screen.queryByRole('button', { name: /^Update all/ })).toBeNull();
	});

	it('stays usable on the desktop', async () => {
		openPkgs();
		const btn = await screen.findByRole('button', { name: 'Update all (1)' });
		expect((btn as HTMLButtonElement).disabled).toBe(false);
	});
});
