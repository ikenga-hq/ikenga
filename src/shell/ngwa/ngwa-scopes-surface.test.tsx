// Ngwa Scopes Surface component tests (WP-16 / locked D-02).

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import { NgwaScopesSurface } from './ngwa-scopes-surface';
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

describe('NgwaScopesSurface component (WP-16 / locked D-02)', () => {
	it('renders matrix with Personal, Project, Claude, Codex columns', () => {
		const items: NgwaItem[] = [
			makeItem({
				id: 'skill-groundwork',
				name: 'groundwork',
				kind: 'skill',
				scope: { kind: 'personal' },
				engines: ['claude'],
			}),
		];

		const { container } = render(<NgwaScopesSurface items={items} activeProjectName="ikenga" />);

		// Header scopes
		expect(screen.getByText('Personal')).not.toBeNull();
		expect(screen.getByText('~/.claude')).not.toBeNull();
		expect(screen.getByText('ikenga')).not.toBeNull();
		expect(screen.getByText('.claude')).not.toBeNull();

		// Header engines
		expect(screen.getByText('claude')).not.toBeNull();
		expect(screen.getByText('codex')).not.toBeNull();

		// Item row
		expect(screen.getByText('groundwork')).not.toBeNull();
		expect(container.querySelector('table.matrix')).not.toBeNull();
	});

	it('P16 Engine Invariant: hides Gemini column and renders footer banner when gemini is not installed', () => {
		const items: NgwaItem[] = [
			makeItem({
				id: 'skill-alpha',
				name: 'alpha',
				kind: 'skill',
				engines: ['claude'],
			}),
		];

		const onInstallEngine = vi.fn();
		const { container } = render(
			<NgwaScopesSurface
				items={items}
				activeProjectName="my-project"
				onInstallEngine={onInstallEngine}
			/>
		);

		// Gemini column is not in header
		const engHeaders = Array.from(container.querySelectorAll('th.eng .colname'));
		const geminiHeader = engHeaders.find((el) => el.textContent === 'gemini');
		expect(geminiHeader).toBeUndefined();

		// P16 footer banner is rendered
		const geminiFoot = container.querySelector('[data-geminifoot]');
		expect(geminiFoot).not.toBeNull();
		expect(geminiFoot?.textContent).toContain(
			'gemini is not installed, so its column is not shown.'
		);

		// Click "Install engine" button in footer
		const installBtn = screen.getByRole('button', { name: /install engine/i });
		fireEvent.click(installBtn);
		expect(onInstallEngine).toHaveBeenCalledWith('gemini');
	});

	it('P16 Engine Invariant: shows Gemini column when gemini engine item is present', () => {
		const items: NgwaItem[] = [
			makeItem({
				id: 'engine-gemini',
				name: 'engine-gemini',
				kind: 'engine',
				engines: ['gemini'],
			}),
			makeItem({
				id: 'skill-alpha',
				name: 'alpha',
				kind: 'skill',
				engines: ['claude', 'gemini'],
			}),
		];

		render(<NgwaScopesSurface items={items} activeProjectName="my-project" />);

		// Gemini column header is shown
		expect(screen.getByText('gemini')).not.toBeNull();

		// Footer banner for missing gemini is NOT rendered
		expect(
			screen.queryByText(/gemini is not installed, so its column is not shown/i)
		).toBeNull();
	});

	it('detects precedence conflicts when same item exists at personal and project with different versions', () => {
		const onPromoteItem = vi.fn();
		const items: NgwaItem[] = [
			makeItem({
				id: 'skill-groundwork-personal',
				name: 'groundwork',
				kind: 'skill',
				version: '1.0.0',
				scope: { kind: 'personal' },
			}),
			makeItem({
				id: 'skill-groundwork-project',
				name: 'groundwork',
				kind: 'skill',
				version: '1.2.0',
				scope: { kind: 'project', project_id: 'proj-1' },
			}),
		];

		render(
			<NgwaScopesSurface
				items={items}
				activeProjectName="ikenga"
				onPromoteItem={onPromoteItem}
			/>
		);

		// Precedence conflict is reported in sidenote drawer
		expect(screen.getByText('groundwork exists twice')).not.toBeNull();
		expect(screen.getByText(/The nearer scope wins/i)).not.toBeNull();

		// Promote action button
		const promoteBtn = screen.getByRole('button', { name: /promote to personal/i });
		fireEvent.click(promoteBtn);
		expect(onPromoteItem).toHaveBeenCalled();
	});

	it('filters rows by kind pill selector', () => {
		const items: NgwaItem[] = [
			makeItem({ id: 'app-studio', name: 'studio', kind: 'app' }),
			makeItem({ id: 'skill-groundwork', name: 'groundwork', kind: 'skill' }),
		];

		render(<NgwaScopesSurface items={items} activeProjectName="ikenga" />);

		expect(screen.getByText('studio')).not.toBeNull();
		expect(screen.getByText('groundwork')).not.toBeNull();

		// Click 'skill' filter
		const skillPills = screen.getAllByRole('button', { name: /skill/i });
		fireEvent.click(skillPills[0]);

		expect(screen.queryByText('studio')).toBeNull();
		expect(screen.getByText('groundwork')).not.toBeNull();
	});

	it('opens cell popover with actions on cell click', () => {
		const onEnableItem = vi.fn();
		const items: NgwaItem[] = [
			makeItem({
				id: 'skill-groundwork',
				name: 'groundwork',
				kind: 'skill',
				state: 'disabled',
				scope: { kind: 'personal' },
			}),
		];

		render(
			<NgwaScopesSurface
				items={items}
				activeProjectName="ikenga"
				onEnableItem={onEnableItem}
			/>
		);

		// Find the cell button for groundwork personal scope: "groundwork in personal scope: disabled"
		const cellBtn = screen.getByLabelText(/groundwork in personal scope/i);
		fireEvent.click(cellBtn);

		// Popover actions appear
		const enableBtn = screen.getByRole('button', { name: /enable in personal/i });
		expect(enableBtn).not.toBeNull();

		// Trigger enable action
		fireEvent.click(enableBtn);
		expect(onEnableItem).toHaveBeenCalled();
	});
});
