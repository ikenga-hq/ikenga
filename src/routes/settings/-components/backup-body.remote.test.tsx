// Gap audit rank 18 — backup export / restore are unserved by the daemon. A
// browser session reads "Not available on this server yet" for those two
// controls and keeps the served local-backups list.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	remote: false,
	backupList: vi.fn(async () => [] as unknown[]),
}));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	isRemoteWebSession: () => h.remote,
	backupList: h.backupList,
}));
vi.mock('@/shell/backup/restore-wizard', () => ({ RestoreWizard: () => null }));

import { BackupSectionBody } from './backup-body';

function renderBody() {
	const qc = new QueryClient();
	return render(
		<QueryClientProvider client={qc}>
			<BackupSectionBody />
		</QueryClientProvider>
	);
}

afterEach(() => {
	cleanup();
	h.remote = false;
	h.backupList.mockClear();
});

describe('BackupSectionBody (gap rank 18)', () => {
	it('replaces the controls with the honest reason in a remote session', () => {
		h.remote = true;
		renderBody();
		expect(screen.getByTestId('backup-unavailable').textContent).toContain(
			'Not available on this server yet'
		);
		expect(screen.queryByRole('button', { name: /Export/ })).toBeNull();
		expect(screen.queryByRole('button', { name: /Restore/ })).toBeNull();
	});

	it('keeps the served list (and its Refresh) in a remote session', async () => {
		h.remote = true;
		renderBody();
		expect(screen.getByText('Local backups')).toBeTruthy();
		await waitFor(() => expect(h.backupList).toHaveBeenCalled());
	});

	it('keeps Export and Restore on the desktop', () => {
		renderBody();
		expect(screen.getByRole('button', { name: /Export/ })).toBeTruthy();
		expect(screen.getByRole('button', { name: /Restore/ })).toBeTruthy();
	});
});
