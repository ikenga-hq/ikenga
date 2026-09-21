// Ngwa Health Surface component tests (WP-16 / locked D-02).

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent, cleanup, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { NgwaHealthSurface } from './ngwa-health-surface';
import type { NgwaItem } from '@ikenga/contract';
import * as tauriCmd from '@/lib/tauri-cmd';

vi.mock('@/lib/tauri-cmd', async (importOriginal) => {
	const actual = await importOriginal<typeof import('@/lib/tauri-cmd')>();
	return {
		...actual,
		pkgPermissionViolationsList: vi.fn().mockResolvedValue([]),
		pkgHealthScan: vi.fn().mockResolvedValue([]),
		dataHealthScan: vi.fn().mockResolvedValue([]),
	};
});

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

function createTestQueryClient() {
	return new QueryClient({
		defaultOptions: {
			queries: {
				retry: false,
			},
		},
	});
}

function makeItem(partial: Partial<NgwaItem>): NgwaItem {
	const id = partial.id ?? 'test-item';
	const name = partial.name ?? id;
	return {
		id,
		kind: 'skill',
		name,
		display_name: partial.display_name ?? name,
		description: 'A test equipment item',
		version: '1.0.0',
		latest_version: null,
		scope: { kind: 'personal' },
		origin: {
			source: 'registry',
			url: null,
			ref: null,
			resolved_version: '1.0.0',
			publisher: null,
			managed: true,
			auto_update: false,
			installed_at_ms: 1000,
			updated_at_ms: 1000,
		},
		state: 'enabled',
		runtime: null,
		trust: {
			state: 'auto_trusted',
			signed: true,
			auto_trusted: true,
			review_pending: false,
			perms: null,
			last_granted_at_ms: null,
		},
		placements: [],
		usage: null,
		requires: [],
		required_by: [],
		owner_pkg_id: null,
		install_path: '/path/to/item',
		engines: ['claude'],
		...partial,
	};
}

describe('NgwaHealthSurface component (WP-16 / locked D-02)', () => {
	it('renders all five consolidated health panels', () => {
		const queryClient = createTestQueryClient();
		const items: NgwaItem[] = [
			makeItem({ id: 'app-studio', name: 'studio', kind: 'app', engines: ['claude'] }),
			makeItem({ id: 'skill-groundwork', name: 'groundwork', kind: 'skill', engines: ['claude'] }),
		];

		const { container } = render(
			<QueryClientProvider client={queryClient}>
				<NgwaHealthSurface items={items} />
			</QueryClientProvider>
		);

		// 1. Violations panel
		expect(container.querySelector('[data-panel="violations"]')).not.toBeNull();

		// 2. Sidecars panel
		expect(container.querySelector('[data-panel="sidecars"]')).not.toBeNull();

		// 3. Cron panel
		expect(container.querySelector('[data-panel="cron"]')).not.toBeNull();

		// 4. Data panel
		expect(container.querySelector('[data-panel="data"]')).not.toBeNull();

		// 5. Engines panel
		expect(container.querySelector('[data-panel="engines"]')).not.toBeNull();
	});

	it('surfaces permission violations when reported by backend', async () => {
		const queryClient = createTestQueryClient();
		vi.mocked(tauriCmd.pkgPermissionViolationsList).mockResolvedValueOnce([
			{
				id: 1,
				pkg_id: 'com.malicious.tool',
				scope_kind: 'fs_write_outside_sandbox',
				attempted: '/etc/hosts',
				declared: '~/.claude/**',
				occurred_at: 1726000000000,
			},
		]);

		const items: NgwaItem[] = [];

		render(
			<QueryClientProvider client={queryClient}>
				<NgwaHealthSurface items={items} />
			</QueryClientProvider>
		);

		await waitFor(() => {
			expect(screen.getByText(/com\.malicious\.tool/)).not.toBeNull();
			expect(screen.getByText(/\/etc\/hosts/)).not.toBeNull();
		});
	});

	it('measures database size when button is clicked in Data panel', async () => {
		const queryClient = createTestQueryClient();
		const items: NgwaItem[] = [];

		render(
			<QueryClientProvider client={queryClient}>
				<NgwaHealthSurface items={items} />
			</QueryClientProvider>
		);

		const measureBtn = screen.getByRole('button', { name: /^measure$/i });
		fireEvent.click(measureBtn);

		await waitFor(() => {
			expect(screen.getByText(/2\.4 MB \(SQLite WAL\)/)).not.toBeNull();
		});
	});

	it('scans dangling soft-FKs on demand when clicked', async () => {
		const queryClient = createTestQueryClient();
		vi.mocked(tauriCmd.dataHealthScan).mockResolvedValueOnce([
			{
				table: 'chat_messages',
				column: 'session_id',
				parent_table: 'sessions',
				orphan_count: 3,
				sample_ids: ['msg-1', 'msg-2', 'msg-3'],
			},
		]);

		const items: NgwaItem[] = [];

		render(
			<QueryClientProvider client={queryClient}>
				<NgwaHealthSurface items={items} />
			</QueryClientProvider>
		);

		const scanBtn = screen.getByRole('button', { name: /^scan$/i });
		fireEvent.click(scanBtn);

		await waitFor(() => {
			expect(tauriCmd.dataHealthScan).toHaveBeenCalled();
			expect(screen.getByText('3')).not.toBeNull();
		});
	});

	it('supports install engine action from Engines panel', () => {
		const queryClient = createTestQueryClient();
		const onInstallEngine = vi.fn();
		const items: NgwaItem[] = [];

		render(
			<QueryClientProvider client={queryClient}>
				<NgwaHealthSurface items={items} onInstallEngine={onInstallEngine} />
			</QueryClientProvider>
		);

		const installGeminiBtn = screen.getByRole('button', { name: /install engine/i });
		fireEvent.click(installGeminiBtn);

		expect(onInstallEngine).toHaveBeenCalledWith('gemini');
	});
});
