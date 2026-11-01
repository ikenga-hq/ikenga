import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useRailAutoStore } from '@/lib/shell/rail-auto-icons';
import { ViewsSection } from './views';

const addTab = vi.fn();
const placeView = vi.fn();
const addPin = vi.fn(async () => {});
let views: unknown[] = [];
let handlersSeen: Array<Record<string, () => void>> = [];

vi.mock('@/lib/pkg/use-activity-bar-entries', () => ({
	usePkgActivityBarEntries: () => ({ views, loaded: true }),
}));
vi.mock('@/lib/panes/pane-store', () => ({
	usePaneStore: { getState: () => ({ focusedId: 'p1', addTab, placeView }) },
}));
vi.mock('@/lib/shell/pins-store', () => ({
	usePinsStore: (sel: (s: { addPin: typeof addPin }) => unknown) => sel({ addPin }),
}));
vi.mock('@/lib/tauri-cmd', () => ({
	settingsGet: async () => null,
	settingsSet: async () => {},
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
const alpha = {
	pkg_id: 'a',
	pkg_name: 'Alpha',
	qualified_id: 'a:v',
	id: 'v',
	title: 'Alpha view',
	route: '/v',
	pane_route: '/pkg/a/v',
	pin_on_install: false,
};

beforeEach(() => {
	views = [];
	handlersSeen = [];
	useRailAutoStore.setState({ hidden: new Set(), hydrated: true });
});
afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('ViewsSection', () => {
	it('no longer lists Outbox approvals (it is a rail icon now)', () => {
		views = [alpha];
		render(<ViewsSection {...ctx} />);
		expect(screen.queryByText('Outbox approvals')).toBeNull();
		expect(screen.queryByTestId('outbox-pending-badge')).toBeNull();
		expect(screen.getByText('Alpha view')).toBeTruthy();
	});

	it('shows the empty state when no pkg contributes a view', () => {
		render(<ViewsSection {...ctx} />);
		expect(screen.getByText('No contributed views')).toBeTruthy();
	});

	it('pkg rows open their pane route and keep their context menu', () => {
		views = [alpha];
		render(<ViewsSection {...ctx} />);
		fireEvent.click(screen.getByText('Alpha view'));
		expect(addTab).toHaveBeenCalledWith('p1', { kind: 'route', path: '/pkg/a/v' });
		expect(Object.keys(handlersSeen[0]!).sort()).toEqual([
			'open',
			'open-in-ngwa',
			'open-to-side',
			'pin-rail',
		]);
	});

	it('offers "Show in rail" only for a pkg whose rail icon is hidden', () => {
		views = [alpha];
		const first = render(<ViewsSection {...ctx} />);
		expect(screen.queryByLabelText('Show Alpha in rail')).toBeNull();
		first.unmount();
		useRailAutoStore.setState({ hidden: new Set(['a']) });
		render(<ViewsSection {...ctx} />);
		fireEvent.click(screen.getByLabelText('Show Alpha in rail'));
		expect(useRailAutoStore.getState().hidden.has('a')).toBe(false);
		expect(addTab).not.toHaveBeenCalled();
	});
});
