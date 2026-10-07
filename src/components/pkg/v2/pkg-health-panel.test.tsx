// PkgHealthPanel on the headless daemon: the scan's `records_unavailable` row
// is a statement about the scan — informational, never counted as broken,
// never removable — and removal itself is desktop-only there.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import * as cmd from '@/lib/tauri-cmd';
import { PkgHealthPanel } from './pkg-health-panel';

vi.mock('@/lib/tauri-cmd', async (orig) => {
	const actual = await orig<typeof import('@/lib/tauri-cmd')>();
	return {
		...actual,
		pkgHealthScan: vi.fn(),
		pkgHealthRemove: vi.fn(),
		pkgHealthRemoveAll: vi.fn(),
		isRemoteWebSession: vi.fn(() => false),
	};
});

const m = vi.mocked(cmd);

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	m.isRemoteWebSession.mockReturnValue(false);
});

const RECORDS: cmd.PkgHealthIssue = {
	id: 'install-records',
	install_path: '',
	enabled: false,
	issue: { kind: 'records_unavailable' },
	detail: 'install-record health is not available on this server: no install records here.',
};

const BROKEN: cmd.PkgHealthIssue = {
	id: 'com.x.broken',
	install_path: '/pkgs/x',
	enabled: false,
	issue: { kind: 'pkgs_dir_unloadable' },
	detail: 'on disk but failed to load: bad json',
};

function mount() {
	const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return render(
		<QueryClientProvider client={qc}>
			<PkgHealthPanel />
		</QueryClientProvider>
	);
}

describe('PkgHealthPanel — records_unavailable', () => {
	it('alone: shown as information, not a broken count, no Remove, never "All installs healthy"', async () => {
		m.isRemoteWebSession.mockReturnValue(true);
		m.pkgHealthScan.mockResolvedValue([RECORDS]);
		const { container } = mount();
		const note = await waitFor(() => {
			const n = container.querySelector('[data-records-unavailable]');
			if (!n) throw new Error('not yet');
			return n as HTMLElement;
		});
		expect(note.textContent).toContain(RECORDS.detail);
		expect(note.querySelector('.text-destructive')).toBeNull();
		expect(container.querySelector('h2 span')).toBeNull();
		expect(container.querySelector('table')).toBeNull();
		expect(container.querySelector('[data-remove]')).toBeNull();
		expect(container.textContent).not.toContain('All installs healthy');
		expect(container.textContent).not.toContain('Remove all');
	});

	it('beside a real issue: counts only the issue, and Remove is desktop-only on the daemon', async () => {
		m.isRemoteWebSession.mockReturnValue(true);
		m.pkgHealthScan.mockResolvedValue([RECORDS, BROKEN]);
		const { container } = mount();
		await waitFor(() =>
			expect(container.querySelector('[data-row="com.x.broken"]')).not.toBeNull()
		);
		expect(container.querySelector('[data-row="install-records"]')).toBeNull();
		expect(container.querySelector('h2 span')?.textContent).toBe('1');
		const rm = container.querySelector<HTMLButtonElement>('[data-remove="com.x.broken"]');
		expect(rm?.disabled).toBe(true);
		expect(rm?.title).toBe('Desktop app only');
	});

	it('on the desktop nothing changes: an issue is removable', async () => {
		m.pkgHealthScan.mockResolvedValue([BROKEN]);
		const { container } = mount();
		await waitFor(() =>
			expect(container.querySelector('[data-row="com.x.broken"]')).not.toBeNull()
		);
		expect(
			container.querySelector<HTMLButtonElement>('[data-remove="com.x.broken"]')?.disabled
		).toBe(false);
		expect(container.querySelector('[data-records-unavailable]')).toBeNull();
	});
});
