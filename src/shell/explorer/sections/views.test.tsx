import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ViewsSection } from './views';

const addTab = vi.fn();
const placeView = vi.fn();
const addPin = vi.fn(async () => {});
let active = true;
let pending = 0;
let views: unknown[] = [];
let handlersSeen: Array<Record<string, () => void>> = [];

vi.mock('@/lib/queries/outbox-approvals-active', () => ({
	useOutboxApprovalsActive: () => active,
}));
vi.mock('@/shell/status-bar', () => ({
	APPROVALS_ROUTE: '/outbox/approvals',
	usePendingApprovalsCount: () => pending,
}));
vi.mock('@/lib/pkg/use-activity-bar-entries', () => ({
	usePkgActivityBarEntries: () => ({ views, loaded: true }),
}));
vi.mock('@/lib/panes/pane-store', () => ({
	usePaneStore: { getState: () => ({ focusedId: 'p1', addTab, placeView }) },
}));
vi.mock('@/lib/shell/pins-store', () => ({
	usePinsStore: (sel: (s: { addPin: typeof addPin }) => unknown) => sel({ addPin }),
}));
vi.mock('@/shell/menu/effective-context-menu', () => ({
	EffectiveContextMenu: ({
		children,
		handlers,
	}: {
		children: ReactNode;
		handlers: Record<string, () => void>;
	}) => {
		handlersSeen.push(handlers);
		return <>{children}</>;
	},
}));

const ctx = { projectId: 'p' };

beforeEach(() => {
	active = true;
	pending = 0;
	views = [];
	handlersSeen = [];
});
afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('ViewsSection Outbox entry', () => {
	it('shows Outbox approvals even when no pkg contributes a view', () => {
		render(<ViewsSection {...ctx} />);
		expect(screen.getByText('Outbox approvals')).toBeTruthy();
		expect(screen.queryByText('No contributed views')).toBeNull();
	});

	it('falls back to the empty state when no backend is active and no views exist', () => {
		active = false;
		render(<ViewsSection {...ctx} />);
		expect(screen.queryByText('Outbox approvals')).toBeNull();
		expect(screen.getByText('No contributed views')).toBeTruthy();
	});

	it('is listed first, above pkg views', () => {
		views = [
			{
				pkg_id: 'a',
				pkg_name: 'Alpha',
				qualified_id: 'a:v',
				id: 'v',
				title: 'Alpha view',
				route: '/v',
				pane_route: '/pkg/a/v',
				pin_on_install: false,
			},
		];
		render(<ViewsSection {...ctx} />);
		const text = document.body.textContent ?? '';
		expect(text.indexOf('Outbox approvals')).toBeLessThan(text.indexOf('Alpha view'));
	});

	it('hides the entry (but keeps pkg views) when the backend is inactive', () => {
		active = false;
		views = [
			{
				pkg_id: 'a',
				pkg_name: 'Alpha',
				qualified_id: 'a:v',
				id: 'v',
				title: 'Alpha view',
				route: '/v',
				pane_route: '/pkg/a/v',
				pin_on_install: false,
			},
		];
		render(<ViewsSection {...ctx} />);
		expect(screen.queryByText('Outbox approvals')).toBeNull();
		expect(screen.getByText('Alpha view')).toBeTruthy();
	});

	it('shows the pending count only when above zero', () => {
		const { unmount } = render(<ViewsSection {...ctx} />);
		expect(screen.queryByTestId('outbox-pending-badge')).toBeNull();
		unmount();
		pending = 3;
		render(<ViewsSection {...ctx} />);
		expect(screen.getByTestId('outbox-pending-badge').textContent).toBe('3');
	});

	it('click opens /outbox/approvals in the focused pane', () => {
		render(<ViewsSection {...ctx} />);
		fireEvent.click(screen.getByText('Outbox approvals'));
		expect(addTab).toHaveBeenCalledWith('p1', {
			kind: 'route',
			path: '/outbox/approvals',
		});
	});

	it('context menu: open-to-side splits, pin-rail pins, no open-in-ngwa', () => {
		render(<ViewsSection {...ctx} />);
		const h = handlersSeen[0]!;
		expect(Object.keys(h).sort()).toEqual(['open', 'open-to-side', 'pin-rail']);
		h['open-to-side']!();
		expect(placeView).toHaveBeenCalledWith(
			'p1',
			{ kind: 'route', path: '/outbox/approvals' },
			'right'
		);
		h['pin-rail']!();
		expect(addPin).toHaveBeenCalledWith(
			expect.objectContaining({
				kind: 'route',
				target: '/outbox/approvals',
				label: 'Outbox approvals',
			})
		);
	});

	it('with the outbound pkg present: one pkg "Outbox" and one shell "Outbox approvals", distinct targets', () => {
		views = [
			{
				pkg_id: 'com.ikenga.outbound',
				pkg_name: 'Outbound',
				qualified_id: 'com.ikenga.outbound:outbound.index',
				id: 'outbound.index',
				title: 'Outbox',
				route: '/',
				pane_route: '/pkg/com.ikenga.outbound/',
				pin_on_install: false,
			},
		];
		render(<ViewsSection {...ctx} />);
		expect(screen.getAllByText('Outbox')).toHaveLength(1);
		expect(screen.getAllByText('Outbox approvals')).toHaveLength(1);
		expect(screen.getByLabelText('Outbox approvals')).toBeTruthy();
		fireEvent.click(screen.getByText('Outbox'));
		expect(addTab).toHaveBeenLastCalledWith('p1', {
			kind: 'route',
			path: '/pkg/com.ikenga.outbound/',
		});
		fireEvent.click(screen.getByText('Outbox approvals'));
		expect(addTab).toHaveBeenLastCalledWith('p1', {
			kind: 'route',
			path: '/outbox/approvals',
		});
	});
});
