import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import { describe, expect, it, afterEach, vi } from 'vitest';
import { NgwaList } from './ngwa-list';
import type { NgwaItem } from '@ikenga/contract';
import type { NgwaAct, NgwaItemActionSet } from '@/lib/ngwa/use-ngwa-actions';

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

	describe('capabilities this server does not run', () => {
		const NA = (what: string) =>
			`${what} is not available on this server: the headless daemon runs no ${what} service`;
		const limited = [
			{ source: 'kernel', error: NA('pkg runtime state'), unavailable: true },
			{ source: 'trust', error: NA('trust evaluation'), unavailable: true },
			{ source: 'engine_assets', error: NA('engine asset placements'), unavailable: true },
			{ source: 'usage', error: 'transcript scan failed: EIO', unavailable: false },
		];

		it('is ONE muted info line, not an error banner per capability', () => {
			const { container } = render(
				<NgwaList items={[makeItem({ id: 'skill-1' })]} unreadableSources={limited} />
			);
			const notes = container.querySelectorAll('[data-server-limits]');
			expect(notes.length).toBe(1);
			expect(notes[0]?.getAttribute('role')).toBe('status');
			expect(notes[0]?.querySelector('[data-server-limits-line]')?.textContent).toBe(
				'On this server: package runtime, trust and engine placement are desktop-only'
			);
			// Nothing about them is styled or announced as an error.
			expect(notes[0]?.className).not.toContain('source-banner');
			const text = notes[0]?.textContent ?? '';
			expect(text).not.toContain('unreadable');
		});

		it('lists each item with its one-line reason in a Details disclosure', () => {
			const { container } = render(
				<NgwaList items={[makeItem({ id: 'skill-1' })]} unreadableSources={limited} />
			);
			const details = container.querySelector('[data-server-limits] details');
			expect(details).not.toBeNull();
			expect(details?.querySelector('summary')?.textContent).toBe('Details');
			const rows = [...(details?.querySelectorAll('li[data-unavailable]') ?? [])];
			expect(rows.map((r) => r.getAttribute('data-unavailable'))).toEqual([
				'kernel',
				'trust',
				'engine_assets',
			]);
			expect(rows[0]?.textContent).toContain('package runtime');
			expect(rows[0]?.textContent).toContain(
				'The headless daemon runs no pkg runtime state service'
			);
		});

		it('a real failure stays a red alert beside the muted line', () => {
			render(<NgwaList items={[makeItem({ id: 'skill-1' })]} unreadableSources={limited} />);
			const alerts = screen.getAllByRole('alert');
			expect(alerts.length).toBe(1);
			expect(alerts[0]?.textContent).toContain('usage unreadable');
			expect(alerts[0]?.textContent).toContain('transcript scan failed: EIO');
			expect(alerts[0]?.className).toContain('source-banner');
		});

		it('the desktop (nothing unavailable) renders no limits note at all', () => {
			const { container } = render(
				<NgwaList
					items={[makeItem({ id: 'skill-1' })]}
					unreadableSources={[{ source: 'oba', error: 'permission denied', unavailable: false }]}
				/>
			);
			expect(container.querySelector('[data-server-limits]')).toBeNull();
			expect(screen.getByRole('alert').textContent).toContain('oba unreadable');
			const { container: clean } = render(<NgwaList items={[makeItem({ id: 'skill-2' })]} />);
			expect(clean.querySelector('[data-server-limits]')).toBeNull();
		});
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

	it('without actions: no action row and no context menu', () => {
		const items = [makeItem({ id: 'a', kind: 'app' })];
		const { container } = render(<NgwaList items={items} />);
		expect(container.querySelector('[data-dacts]')).toBeNull();
		fireEvent.contextMenu(container.querySelector('[data-id="a"]') as HTMLElement);
		expect(screen.queryByRole('menu')).toBeNull();
	});

	it('with actions: Space toggles, Enter opens the folder, double-click opens the item', () => {
		const items = [makeItem({ id: 'a', kind: 'app' })];
		const set = stubActions();
		const onOpenItem = vi.fn();
		const { container } = render(
			<NgwaList items={items} actionsFor={() => set} onOpenItem={onOpenItem} />
		);
		const row = container.querySelector('[data-id="a"]') as HTMLElement;
		fireEvent.keyDown(row, { key: ' ' });
		expect(set.toggle.run).toHaveBeenCalledTimes(1);
		fireEvent.keyDown(row, { key: 'Enter' });
		expect(set.openFolder.run).toHaveBeenCalledTimes(1);
		fireEvent.doubleClick(row);
		expect(onOpenItem).toHaveBeenCalledWith(items[0]);
	});

	it('the context menu runs the picked action and closes', () => {
		const items = [makeItem({ id: 'a', kind: 'app', display_name: 'Alpha' })];
		const set = stubActions();
		const { container } = render(<NgwaList items={items} actionsFor={() => set} />);
		fireEvent.contextMenu(container.querySelector('[data-id="a"]') as HTMLElement, {
			clientX: 40,
			clientY: 60,
		});
		const menu = screen.getByRole('menu', { name: 'Alpha' });
		expect(menu.style.left).toBe('40px');
		const remove = screen.getByRole('menuitem', { name: 'Remove…' });
		expect(remove.className).toContain('danger');
		fireEvent.click(remove);
		expect(set.remove.run).toHaveBeenCalledTimes(1);
		expect(screen.queryByRole('menu')).toBeNull();
	});
});

function act(label: string, disabledReason?: string): NgwaAct {
	return { label, disabledReason, run: vi.fn() };
}

function stubActions(): NgwaItemActionSet {
	return {
		toggle: act('Disable'),
		move: { title: 'Move to', targets: [] },
		copy: { title: 'Copy to', targets: [] },
		moveToProject: act('Move to project'),
		moveToPersonal: act('Move to personal', 'Already personal'),
		update: act('Update', '1.0.0 is the newest published version'),
		openFolder: act('Open folder'),
		remove: act('Remove…'),
		handToChi: act('Hand to Chi'),
		openView: null,
		openManifest: act('Open manifest.json'),
		revealInstallPath: act('Reveal install path'),
		resetSettings: act('Reset settings to defaults'),
		copyIyke: act('Copy as iyke'),
		iyke: 'iyke ngwa item a',
		briefChi: null,
	};
}
