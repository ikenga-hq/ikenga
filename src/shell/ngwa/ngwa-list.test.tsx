import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import { describe, expect, it, afterEach } from 'vitest';
import { NgwaList } from './ngwa-list';
import type { NgwaItem } from '@ikenga/contract';

afterEach(() => {
	cleanup();
});

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

describe('NgwaList component (D-02 / WP-15)', () => {
	it('renders list with equipment items', () => {
		const items = [
			makeItem({ id: 'skill-alpha', kind: 'skill', display_name: 'Skill Alpha' }),
			makeItem({ id: 'app-beta', kind: 'app', display_name: 'App Beta' }),
		];
		const { container } = render(<NgwaList items={items} />);
		expect(container.querySelector('[data-id="skill-alpha"]')).not.toBeNull();
		expect(container.querySelector('[data-id="app-beta"]')).not.toBeNull();
	});

	it('renders unmeasured usage (null) as em-dash "—", never as "0"', () => {
		const items = [
			makeItem({
				id: 'unmeasured-item',
				name: 'unmeasured-item',
				usage: null,
			}),
		];
		const { container } = render(<NgwaList items={items} />);
		const usageEl = container.querySelector('[data-usage-value]');
		expect(usageEl).toBeDefined();
		expect(usageEl?.textContent).toBe('—');
		expect(usageEl?.textContent).not.toBe('0');
		expect(usageEl?.textContent).not.toBe('0 sessions');
	});

	it('renders measured zero usage as "0 sessions", distinguishable from "—"', () => {
		const items = [
			makeItem({
				id: 'zero-item',
				name: 'zero-item',
				usage: {
					source: 'transcript',
					last_used_ms: null,
					count_7d: 0,
					count_30d: 0,
					tokens_30d: 0,
					window_start_ms: 1000,
				},
			}),
		];
		const { container } = render(<NgwaList items={items} />);
		const usageEl = container.querySelector('[data-usage-value]');
		expect(usageEl).toBeDefined();
		expect(usageEl?.textContent).toBe('0 sessions');
		expect(usageEl?.textContent).not.toBe('—');
		expect(usageEl?.textContent).not.toBe('0');
	});

	it('surfaces unreadable sources banner when source ok is false (Gate §2)', () => {
		const items = [makeItem({ id: 'skill-1' })];
		render(
			<NgwaList
				items={items}
				unreadableSources={[{ source: 'oba', error: 'permission denied reading store' }]}
			/>
		);
		const banner = screen.getByRole('alert');
		expect(banner.textContent).toContain('oba unreadable');
		expect(banner.textContent).toContain('permission denied reading store');
	});

	it('groups closure child items under parent pkg and shows group chip', () => {
		const parent = makeItem({
			id: 'pkg-git',
			name: 'pkg-git',
			display_name: 'Git Package',
			kind: 'app',
		});
		const child = makeItem({
			id: 'git-status-skill',
			name: 'git-status-skill',
			display_name: 'Git Status',
			kind: 'skill',
			owner_pkg_id: 'pkg-git',
		});

		const { container } = render(<NgwaList items={[parent, child]} />);

		// Group chip should be visible on child row
		const grpChip = container.querySelector('.grpchip');
		expect(grpChip).toBeDefined();
		expect(grpChip?.textContent).toBe('pkg-git');

		const childRow = container.querySelector('.irow.child');
		expect(childRow).toBeDefined();
	});

	it('filters list by search query', () => {
		const items = [
			makeItem({ id: 'groundwork', name: 'groundwork', display_name: 'Groundwork Skill' }),
			makeItem({ id: 'brand-voice', name: 'brand-voice', display_name: 'Brand Voice Skill' }),
		];
		const { container } = render(<NgwaList items={items} />);

		const searchInput = screen.getByPlaceholderText('Filter by name or path…');
		fireEvent.change(searchInput, { target: { value: 'groundwork' } });

		expect(container.querySelector('[data-id="groundwork"]')).not.toBeNull();
		expect(container.querySelector('[data-id="brand-voice"]')).toBeNull();
	});
});
