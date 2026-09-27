// WP-69 — D-09 `popout` in the thin window: "Window 2 holds two tabs;
// clicking one switches", a join adds a tab live, a move back removes one.
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const m = vi.hoisted(() => ({
	handlers: new Map<string, (ev: { event: string; payload: unknown }) => void>(),
	listWindows: vi.fn(async (): Promise<unknown[]> => []),
}));

vi.mock('@/lib/transport', () => ({
	isTauri: () => true,
	listen: vi.fn(async (topic: string, h: (ev: { event: string; payload: unknown }) => void) => {
		m.handlers.set(topic, h);
		return () => m.handlers.delete(topic);
	}),
}));
vi.mock('@/lib/tauri-cmd', () => ({ listWindows: m.listWindows }));
vi.mock('./window-actions', () => ({
	default: ({ surfaceId }: { surfaceId: string | null }) => <button type="button">actions {surfaceId}</button>,
}));
vi.mock('./surfaces/surface-tab-label', () => ({
	default: ({ surfaceId }: { surfaceId: string }) => <span>tab {surfaceId}</span>,
}));
vi.mock('./registry', () => {
	const Body = ({ ctx, actions }: { ctx: { surfaces: string[] }; actions?: unknown }) => (
		<div data-testid="body">
			body {ctx.surfaces.join(',')}
			{actions as never}
		</div>
	);
	return {
		registeredSurfaceIds: () => ['terminal'],
		resolveSurface: (id: string) =>
			id.startsWith('terminal:') ? { id: 'terminal', title: 'Terminal', component: Body, ownsActions: true } : undefined,
	};
});

import type { WindowContext } from '@/lib/window/window-context';
import { DetachedRoot } from './detached-root';

const ctx: WindowContext = {
	label: 'detached-w2',
	kind: 'single-surface',
	surfaces: ['terminal:a'],
	projectId: null,
	isDetached: true,
};

function emitChange(surface_set: string[], added: string[] = [], removed: string[] = []) {
	const h = m.handlers.get('window://surfaces-changed');
	if (!h) throw new Error('no surfaces-changed listener');
	act(() =>
		h({
			event: 'window://surfaces-changed',
			payload: {
				v: 1,
				topic: 'window://surfaces-changed',
				source_label: 'core',
				target: { kind: 'window', label: 'detached-w2' },
				payload: { label: 'detached-w2', surface_set, added, removed, move_back: removed.length > 0 },
			},
		})
	);
}

beforeEach(() => {
	m.handlers.clear();
	m.listWindows.mockReset().mockResolvedValue([]);
});
afterEach(cleanup);

describe('DetachedRoot — Window 2 with tabs (D-09 popout)', () => {
	it('one surface: no tab strip; the ⋯ rides the surface header', async () => {
		render(<DetachedRoot ctx={ctx} />);
		expect(await screen.findByTestId('body')).toHaveProperty('textContent', expect.stringContaining('body terminal:a'));
		expect(screen.queryByRole('tablist')).toBeNull();
		expect(await screen.findByText('actions terminal:a')).toBeTruthy();
		expect(document.querySelector('[data-state="window-single"]')).not.toBeNull();
	});

	it('a join adds a tab and activates it; clicking a tab switches', async () => {
		render(<DetachedRoot ctx={ctx} />);
		await waitFor(() => expect(m.handlers.has('window://surfaces-changed')).toBe(true));
		emitChange(['terminal:a', 'terminal:b'], ['terminal:b']);

		const tabs = await screen.findAllByRole('tab');
		expect(tabs).toHaveLength(2);
		expect(screen.getByTestId('body').textContent).toContain('body terminal:b');
		expect(document.querySelector('[data-state="window-tabs"]')).not.toBeNull();

		fireEvent.click(tabs[0]);
		await waitFor(() => expect(screen.getByTestId('body').textContent).toContain('body terminal:a'));
	});

	it('a move back removes the tab; the strip merges away at one', async () => {
		render(<DetachedRoot ctx={{ ...ctx, surfaces: ['terminal:a', 'terminal:b'] }} />);
		await waitFor(() => expect(m.handlers.has('window://surfaces-changed')).toBe(true));
		expect(screen.getAllByRole('tab')).toHaveLength(2);
		emitChange(['terminal:a'], [], ['terminal:b']);
		await waitFor(() => expect(screen.queryByRole('tablist')).toBeNull());
		expect(screen.getByTestId('body').textContent).toContain('body terminal:a');
	});

	it('ignores another window’s change', async () => {
		render(<DetachedRoot ctx={ctx} />);
		await waitFor(() => expect(m.handlers.has('window://surfaces-changed')).toBe(true));
		const h = m.handlers.get('window://surfaces-changed');
		act(() =>
			h?.({
				event: 'window://surfaces-changed',
				payload: { payload: { label: 'detached-other', surface_set: ['terminal:z'], added: ['terminal:z'], removed: [], move_back: false } },
			})
		);
		expect(screen.queryByRole('tablist')).toBeNull();
	});

	it('reconciles from the registry when a join landed before the listener', async () => {
		m.listWindows.mockResolvedValue([
			{ label: 'detached-w2', kind: 'single-surface', surface_set: ['terminal:a', 'terminal:c'], project_id: null, layout_key: 'detached-w2' },
		]);
		render(<DetachedRoot ctx={ctx} />);
		expect(await screen.findAllByRole('tab')).toHaveLength(2);
		expect(screen.getByTestId('body').textContent).toContain('body terminal:c');
	});
});
