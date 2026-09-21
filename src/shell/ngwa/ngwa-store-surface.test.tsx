// Ngwa Store Surface component tests (WP-15 / locked D-02).

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import { NgwaStoreSurface } from './ngwa-store-surface';
import type { NgwaStoreEntry } from '@/lib/ngwa/enrichment';

afterEach(() => {
	cleanup();
});

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
			kind: 'app',
			scope: { kind: 'personal', project_id: null },
			origin: { source: 'pkg', path: null },
			version: '0.8.2',
			state: 'update',
			engines: ['claude'],
			latest_version: '0.8.3',
			usage: {
				session_count: 5,
				last_used: '2026-09-20T10:00:00Z',
				total_duration_secs: 120,
				total_input_tokens: 100,
				total_output_tokens: 50,
			},
			owner_pkg_id: null,
			required_by: [],
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
		const { container } = render(<NgwaStoreSurface catalog={mockCatalog} />);
		expect(screen.getAllByText('pkg-tasks').length).toBeGreaterThanOrEqual(1);
		expect(screen.getAllByText('skill-groundwork').length).toBeGreaterThanOrEqual(1);
		expect(container.querySelector('.updates')).not.toBeNull();
	});

	it('renders updates banner with correct update count and details', () => {
		render(<NgwaStoreSurface catalog={mockCatalog} />);
		expect(screen.getByText(/1 update available/i)).toBeDefined();
		expect(screen.getAllByText(/0.8.2 → 0.8.3/i).length).toBeGreaterThanOrEqual(1);
	});

	it('filters entries by search query', () => {
		render(<NgwaStoreSurface catalog={mockCatalog} />);
		const searchInput = screen.getByPlaceholderText('Search the registry…');
		fireEvent.change(searchInput, { target: { value: 'groundwork' } });

		expect(screen.getAllByText('skill-groundwork').length).toBeGreaterThanOrEqual(1);
		expect(screen.queryByText('pkg-tasks')).toBeNull();
	});

	it('filters entries by kind facet chip', () => {
		const { container } = render(<NgwaStoreSurface catalog={mockCatalog} />);
		const skillChip = container.querySelector('button[data-kind="skill"]') as HTMLElement;
		expect(skillChip).toBeDefined();
		fireEvent.click(skillChip);

		expect(screen.getAllByText('skill-groundwork').length).toBeGreaterThanOrEqual(1);
		expect(screen.queryByText('pkg-tasks')).toBeNull();
	});

	it('handles update and install callbacks', () => {
		const onUpdate = vi.fn();
		const onInstall = vi.fn();
		render(
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
