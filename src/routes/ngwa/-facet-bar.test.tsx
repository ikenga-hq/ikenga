// NgwaFacetBar tests — verify facet parity with deleted ngwa-mode.tsx (WP-10).

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import { NgwaFacetBar, type NgwaSearchParams } from './-facet-bar';

afterEach(() => {
	cleanup();
});

// Mock useNavigate so we don't need full TanStack router harness for basic rendering
vi.mock('@tanstack/react-router', () => ({
	useNavigate: () => vi.fn(),
	useLocation: () => ({ pathname: '/ngwa/installed' }),
}));

describe('NgwaFacetBar', () => {
	it('renders all surfaces, scopes, engines, and kinds with correct data attributes', () => {
		const search: NgwaSearchParams = {
			surface: 'browse',
			scope: 'all',
			kind: 'skills',
			sys: undefined,
		};

		render(<NgwaFacetBar search={search} />);

		// Surfaces
		expect(screen.getByRole('button', { name: 'Browse' }).getAttribute('data-surface')).toBe('browse');
		expect(screen.getByRole('button', { name: 'Registry' }).getAttribute('data-surface')).toBe('registry');
		expect(screen.getByRole('button', { name: 'Store' }).getAttribute('data-surface')).toBe('store');
		expect(screen.getByRole('button', { name: 'Graph' }).getAttribute('data-surface')).toBe('graph');
		expect(screen.getByRole('button', { name: 'Map' }).getAttribute('data-surface')).toBe('map');
		expect(screen.getByRole('button', { name: 'Life' }).getAttribute('data-surface')).toBe('life');
		expect(screen.getByRole('button', { name: 'Health' }).getAttribute('data-surface')).toBe('health');
		expect(screen.getByRole('button', { name: 'Flow' }).getAttribute('data-surface')).toBe('flow');

		// Scopes
		expect(screen.getByRole('button', { name: 'All' }).getAttribute('data-scope')).toBe('all');
		expect(screen.getByRole('button', { name: 'Personal' }).getAttribute('data-scope')).toBe('personal');

		// Engines
		expect(screen.getByRole('button', { name: /Claude/i }).getAttribute('data-system')).toBe('claude');
		expect(screen.getByRole('button', { name: /Gemini/i }).getAttribute('data-system')).toBe('gemini');
		expect(screen.getByRole('button', { name: /Codex/i }).getAttribute('data-system')).toBe('codex');

		// Kinds
		expect(screen.getByRole('button', { name: 'Skills' }).getAttribute('data-kind')).toBe('skills');
		expect(screen.getByRole('button', { name: 'Agents' }).getAttribute('data-kind')).toBe('agents');
		expect(screen.getByRole('button', { name: 'Commands' }).getAttribute('data-kind')).toBe('commands');
		expect(screen.getByRole('button', { name: 'Hooks' }).getAttribute('data-kind')).toBe('hooks');
		expect(screen.getByRole('button', { name: 'MCPs' }).getAttribute('data-kind')).toBe('mcps');
	});

	it('triggers search update on surface click', () => {
		const onSearchChange = vi.fn();
		const search: NgwaSearchParams = { surface: 'browse' };

		render(<NgwaFacetBar search={search} onSearchChange={onSearchChange} />);

		fireEvent.click(screen.getByRole('button', { name: 'Registry' }));
		expect(onSearchChange).toHaveBeenCalledWith(
			expect.objectContaining({ surface: 'registry' })
		);

		fireEvent.click(screen.getByRole('button', { name: 'Health' }));
		expect(onSearchChange).toHaveBeenCalledWith(
			expect.objectContaining({ surface: 'health' })
		);
	});

	it('triggers search update on scope click', () => {
		const onSearchChange = vi.fn();
		const search: NgwaSearchParams = { scope: 'all' };

		render(<NgwaFacetBar search={search} onSearchChange={onSearchChange} />);

		fireEvent.click(screen.getByRole('button', { name: 'Personal' }));
		expect(onSearchChange).toHaveBeenCalledWith(
			expect.objectContaining({ scope: 'personal' })
		);
	});

	it('triggers search update on kind click', () => {
		const onSearchChange = vi.fn();
		const search: NgwaSearchParams = { kind: 'skills' };

		render(<NgwaFacetBar search={search} onSearchChange={onSearchChange} />);

		fireEvent.click(screen.getByRole('button', { name: 'Agents' }));
		expect(onSearchChange).toHaveBeenCalledWith(
			expect.objectContaining({ kind: 'agents' })
		);
	});

	it('toggles engine in sys parameter', () => {
		const onSearchChange = vi.fn();
		// Initial state: all 3 active (sys undefined means all active)
		const search: NgwaSearchParams = { sys: undefined };

		render(<NgwaFacetBar search={search} onSearchChange={onSearchChange} />);

		// Clicking Claude toggles it off -> only gemini,codex remain
		fireEvent.click(screen.getByRole('button', { name: /Claude/i }));
		expect(onSearchChange).toHaveBeenCalledWith(
			expect.objectContaining({ sys: 'gemini,codex' })
		);
	});

	it('dims and disables kind selector on analyze surfaces', () => {
		const search: NgwaSearchParams = { surface: 'graph', kind: 'skills' };

		render(<NgwaFacetBar search={search} />);

		const skillsButton = screen.getByRole('button', { name: 'Skills' });
		expect(skillsButton.hasAttribute('disabled')).toBe(true);
	});
});
