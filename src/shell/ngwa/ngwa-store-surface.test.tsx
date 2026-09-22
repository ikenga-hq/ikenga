// Ngwa Store Surface component tests (WP-15 / locked D-02).

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { NgwaStoreSurface } from './ngwa-store-surface';
import type { NgwaStoreEntry } from '@/lib/ngwa/enrichment';

afterEach(() => {
	cleanup();
});

function renderWithClient(ui: React.ReactElement) {
	const client = new QueryClient({
		defaultOptions: { queries: { retry: false } },
	});
	return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

const mockCatalog: NgwaStoreEntry[] = [
	{
		id: '@ikenga/pkg-tasks',
		name: '@ikenga/pkg-tasks',
		displayName: 'pkg-tasks',
		description: 'Task management package',
		version: '0.8.2',
		latestVersion: '0.8.3',
		kind: 'app',
		trustFacet: 'signed',
		installedItem: {
			id: '@ikenga/pkg-tasks',
			name: '@ikenga/pkg-tasks',
			display_name: 'pkg-tasks',
			description: 'Task management package',
			version: '0.8.2',
			latest_version: '0.8.3',
			kind: 'app',
			scope: { kind: 'personal' },
			origin: {
				source: 'registry',
				url: 'https://registry.ikenga.dev/index.json',
				ref: null,
				resolved_version: '0.8.2',
				publisher: 'ikenga-hq',
				managed: true,
				auto_update: false,
				installed_at_ms: 1_726_000_000_000,
				updated_at_ms: 1_726_900_000_000,
			},
			state: 'update',
			runtime: null,
			trust: {
				state: 'granted',
				signed: true,
				auto_trusted: false,
				review_pending: false,
				perms: null,
				last_granted_at_ms: 1_726_900_000_000,
			},
			placements: [],
			usage: {
				source: 'transcript',
				last_used_ms: 1_726_950_000_000,
				count_7d: 5,
				count_30d: 20,
				tokens_30d: 150,
				window_start_ms: 1_724_000_000_000,
			},
			requires: [],
			required_by: [],
			owner_pkg_id: null,
			install_path: '/Users/x/.ikenga/pkgs/pkg-tasks',
			engines: ['claude'],
		},
		isUpdate: true,
		registryEntry: {
			name: '@ikenga/pkg-tasks',
			latest: '0.8.3',
			detail: 'https://example.com/tasks.json',
		},
	},
	{
		id: 'skill-groundwork',
		name: 'skill-groundwork',
		displayName: 'skill-groundwork',
		description: 'Foundational prompt skill',
		version: '1.0.0',
		latestVersion: '1.0.0',
		kind: 'skill',
		trustFacet: 'signed',
		installedItem: null,
		isUpdate: false,
		registryEntry: {
			name: 'skill-groundwork',
			latest: '1.0.0',
			detail: 'https://example.com/gw.json',
		},
	},
];

describe('NgwaStoreSurface', () => {
	it('renders store catalog items', () => {
		const { container } = renderWithClient(<NgwaStoreSurface catalog={mockCatalog} />);
		expect(screen.getAllByText('pkg-tasks').length).toBeGreaterThanOrEqual(1);
		expect(screen.getAllByText('skill-groundwork').length).toBeGreaterThanOrEqual(1);
		expect(container.querySelector('.updates')).not.toBeNull();
	});

	it('renders updates banner with correct update count and details', () => {
		renderWithClient(<NgwaStoreSurface catalog={mockCatalog} />);
		expect(screen.getByText(/1 update available/i)).toBeDefined();
		expect(screen.getAllByText(/0.8.2 → 0.8.3/i).length).toBeGreaterThanOrEqual(1);
	});

	it('filters entries by search query', () => {
		renderWithClient(<NgwaStoreSurface catalog={mockCatalog} />);
		const searchInput = screen.getByPlaceholderText('Search the registry…');
		fireEvent.change(searchInput, { target: { value: 'groundwork' } });

		expect(screen.getAllByText('skill-groundwork').length).toBeGreaterThanOrEqual(1);
		expect(screen.queryByText('pkg-tasks')).toBeNull();
	});

	it('filters entries by kind facet chip', () => {
		const { container } = renderWithClient(<NgwaStoreSurface catalog={mockCatalog} />);
		const skillChip = container.querySelector('button[data-kind="skill"]') as HTMLElement;
		expect(skillChip).toBeDefined();
		fireEvent.click(skillChip);

		expect(screen.getAllByText('skill-groundwork').length).toBeGreaterThanOrEqual(1);
		expect(screen.queryByText('pkg-tasks')).toBeNull();
	});

	it('handles update and install callbacks', () => {
		const onUpdate = vi.fn();
		const onInstall = vi.fn();
		renderWithClient(
			<NgwaStoreSurface
				catalog={mockCatalog}
				onUpdate={onUpdate}
				onInstall={onInstall}
			/>
		);

		const updateBtn = screen.getByRole('button', { name: /^update/i });
		fireEvent.click(updateBtn);
		expect(onUpdate).toHaveBeenCalledWith(mockCatalog[0]);

		// Click install dropdown arrow on groundwork
		const chevronBtn = screen.getByLabelText('Choose install scope');
		fireEvent.click(chevronBtn);

		const personalOption = screen.getByText(/Personal scope/i);
		fireEvent.click(personalOption);
		expect(onInstall).toHaveBeenCalledWith(mockCatalog[1], 'personal');
	});
});
