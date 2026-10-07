// Gap audit rank 3 — onboarding's Ngwa step in a browser session: the daemon
// serves no package install, so the step says so up front and Continue records
// every pick as NOT installed (with the reason) instead of leaving the Done
// step to guess. The desktop path still runs the batch.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ remote: false, triggerPkgInstalls: vi.fn() }));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	isRemoteWebSession: () => h.remote,
}));
vi.mock('@/lib/onboarding/install-queue', async (orig) => ({
	...(await orig<typeof import('@/lib/onboarding/install-queue')>()),
	prewarmCatalog: vi.fn(async () => {}),
	triggerPkgInstalls: h.triggerPkgInstalls,
}));

import { useShellStore } from '@/lib/shell/shell-store';

import { EquipmentBody, type EquipmentStepPayload } from './equipment-body';

const PICK = 'com.ikenga.tasks';

function renderBody() {
	return render(
		<QueryClientProvider client={new QueryClient()}>
			<EquipmentBody onContinue={() => {}} />
		</QueryClientProvider>
	);
}
const payload = () =>
	useShellStore.getState().onboarding.steps.equipment.payload as EquipmentStepPayload | undefined;

beforeEach(() => {
	useShellStore.getState().setOnboardingPayload('equipment', {
		selected: [PICK],
		connectorsConfigured: [],
		connectorsSkipped: [],
	} satisfies EquipmentStepPayload);
});
afterEach(() => {
	cleanup();
	h.remote = false;
	h.triggerPkgInstalls.mockReset();
});

describe('EquipmentBody installs (gap rank 3)', () => {
	it('says installs are unavailable and records the picks as not installed', async () => {
		h.remote = true;
		renderBody();
		expect(screen.getByTestId('equipment-install-unavailable').textContent).toContain(
			'Not available on this server yet'
		);
		fireEvent.click(screen.getByTestId('equipment-inline-continue'));
		await waitFor(() => expect(payload()?.installResults).toBeDefined());
		expect(payload()?.installResults).toEqual([
			expect.objectContaining({
				pkgId: PICK,
				ok: false,
				error: 'Not available on this server yet',
			}),
		]);
		expect(h.triggerPkgInstalls).not.toHaveBeenCalled();
	});

	it('on the desktop shows no notice and runs the install batch', async () => {
		h.triggerPkgInstalls.mockResolvedValue([
			{ pkgId: PICK, display: 'Tasks', ok: true, skipped: false },
		]);
		renderBody();
		expect(screen.queryByTestId('equipment-install-unavailable')).toBeNull();
		fireEvent.click(screen.getByTestId('equipment-inline-continue'));
		await waitFor(() => expect(h.triggerPkgInstalls).toHaveBeenCalledTimes(1));
	});

	it('a batch that throws is recorded as failed, never left unknown', async () => {
		h.triggerPkgInstalls.mockRejectedValue(new Error('registry exploded'));
		renderBody();
		fireEvent.click(screen.getByTestId('equipment-inline-continue'));
		await waitFor(() => expect(payload()?.installResults).toBeDefined());
		expect(payload()?.installResults).toEqual([
			expect.objectContaining({ pkgId: PICK, ok: false, error: 'registry exploded' }),
		]);
	});
});
