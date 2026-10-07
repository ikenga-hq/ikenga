import { render, screen, cleanup, waitFor, renderHook, fireEvent } from '@testing-library/react';
import { describe, expect, it, afterEach, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import type { NgwaSnapshot } from '@ikenga/contract';
import * as tauriCmd from '@/lib/tauri-cmd';
import { dismissToast, useToastStore } from '@/lib/toast';
import { NgwaProjectSection, useNgwaProjectRowCount } from './ngwa-project';
import { mkItem, mkSnapshot } from '@/routes/ngwa/-ngwa-test-fixtures';

const addTab = vi.fn();

vi.mock('@/lib/registry/use-registry', () => ({
	useRegistryIndex: () => ({ data: undefined, isLoading: false, error: null }),
}));

vi.mock('@/lib/tauri-cmd', async (orig) => {
	const actual = await orig<typeof import('@/lib/tauri-cmd')>();
	return {
		...actual,
		pkgKernelStatus: vi.fn(),
		pkgSetEnabled: vi.fn(),
		pkgUninstall: vi.fn(),
		ngwaSnapshot: vi.fn(),
	};
});

vi.mock('@/lib/panes/pane-store', () => ({
	usePaneStore: { getState: () => ({ focusedId: 'p1', addTab }) },
}));

const m = vi.mocked(tauriCmd);

afterEach(() => {
	cleanup();
	vi.resetAllMocks();
});

/** A promise that never settles — holds `useNgwaSnapshot()` in `isLoading`. */
function pendingForever<T>(): Promise<T> {
	return new Promise<T>(() => {});
}

function wrapper() {
	const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	// biome-ignore lint/suspicious/noExplicitAny: test harness wrapper
	const W = ({ children }: { children: ReactNode }) => (
		<QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
	);
	return W;
}

function renderSection(projectId: string) {
	const Wrapper = wrapper();
	return render(
		<Wrapper>
			<NgwaProjectSection projectId={projectId} />
		</Wrapper>
	);
}

function kernelStatus(pkgs: Array<{ id: string; version?: string; project_id?: string | null }>) {
	return {
		api_version: 5,
		registries: {},
		installed: pkgs.map((p) => ({
			id: p.id,
			version: p.version ?? '1.0.0',
			ikenga_api: '5',
			install_path: `/pkgs/${p.id}`,
			enabled: true,
			installed_at: 0,
			compatible: true,
			source: { kind: 'local' },
			project_id: p.project_id ?? null,
		})),
	} as never;
}

describe('NgwaProjectSection — fallback while the Ngwa snapshot loads', () => {
	it.each([['com.ikenga.engine-claude-code'], ['com.ikenga.iyke-skill'], ['com.ikenga.studio']])(
		'opens the Ngwa item detail route for %s, not /pkg/<id>',
		async (pkgId) => {
			m.ngwaSnapshot.mockReturnValue(pendingForever());
			m.pkgKernelStatus.mockResolvedValue(kernelStatus([{ id: pkgId }]));

			renderSection('proj-1');

			const row = await screen.findByText(pkgId);
			row.click();

			expect(addTab).toHaveBeenCalledWith('p1', {
				kind: 'route',
				path: `/ngwa/item/${encodeURIComponent(pkgId)}`,
			});
			expect(addTab).not.toHaveBeenCalledWith(
				'p1',
				expect.objectContaining({ path: `/pkg/${pkgId}` })
			);
		}
	);

	it('includes pkgs scoped to the active project and project-less pkgs, excludes other projects', async () => {
		m.ngwaSnapshot.mockReturnValue(pendingForever());
		m.pkgKernelStatus.mockResolvedValue(
			kernelStatus([
				{ id: 'com.ikenga.global-tool', project_id: null },
				{ id: 'com.ikenga.this-project', project_id: 'proj-1' },
				{ id: 'com.ikenga.other-project', project_id: 'proj-2' },
			])
		);

		renderSection('proj-1');

		await waitFor(() => expect(screen.getByText('com.ikenga.global-tool')).toBeDefined());
		expect(screen.getByText('com.ikenga.this-project')).toBeDefined();
		expect(screen.queryByText('com.ikenga.other-project')).toBeNull();
	});
});

describe('NgwaProjectSection — snapshot rows (DEC-74)', () => {
	it('renders this project’s mixed-kind Ngwa items with a muted kind meta, not kernel pkgs', async () => {
		m.pkgKernelStatus.mockResolvedValue(kernelStatus([{ id: 'com.ikenga.should-not-show' }]));
		m.ngwaSnapshot.mockResolvedValue(
			mkSnapshot([
				mkItem({
					id: 'skill:project:proj-1:groundwork',
					kind: 'skill',
					name: 'groundwork',
					scope: { kind: 'project', project_id: 'proj-1' },
				}),
				mkItem({
					id: 'hook:project:proj-1:secret-scan',
					kind: 'hook',
					name: 'secret-scan',
					display_name: 'secret-scan',
					scope: { kind: 'project', project_id: 'proj-1' },
				}),
				// A different project — must not show.
				mkItem({
					id: 'agent:project:proj-2:explore',
					kind: 'agent',
					name: 'explore',
					scope: { kind: 'project', project_id: 'proj-2' },
				}),
			])
		);

		renderSection('proj-1');

		await waitFor(() => expect(screen.getByText('groundwork')).toBeDefined());
		expect(screen.getByText('secret-scan')).toBeDefined();
		expect(screen.queryByText('explore')).toBeNull();
		expect(screen.queryByText('com.ikenga.should-not-show')).toBeNull();

		// Muted kind meta renders alongside each row.
		expect(screen.getAllByText('skill')).toHaveLength(1);
		expect(screen.getAllByText('hook')).toHaveLength(1);
	});

	it("shows this project's personal-scope items when the active project is 'default' (DEC-71)", async () => {
		m.pkgKernelStatus.mockResolvedValue(kernelStatus([]));
		m.ngwaSnapshot.mockResolvedValue(
			mkSnapshot([
				mkItem({ id: 'skill:personal:groundwork', kind: 'skill', name: 'groundwork' }),
				mkItem({
					id: 'skill:project:proj-1:other',
					kind: 'skill',
					name: 'other',
					scope: { kind: 'project', project_id: 'proj-1' },
				}),
			])
		);

		renderSection('default');

		await waitFor(() => expect(screen.getByText('groundwork')).toBeDefined());
		expect(screen.queryByText('other')).toBeNull();
	});

	it('falls back to kernel pkgs while loading, then swaps to snapshot items once the scan resolves', async () => {
		let resolveSnapshot: (v: NgwaSnapshot) => void = () => {};
		m.ngwaSnapshot.mockReturnValue(
			new Promise<NgwaSnapshot>((resolve) => {
				resolveSnapshot = resolve;
			})
		);
		m.pkgKernelStatus.mockResolvedValue(kernelStatus([{ id: 'com.ikenga.fallback-pkg' }]));

		renderSection('proj-1');

		await waitFor(() => expect(screen.getByText('com.ikenga.fallback-pkg')).toBeDefined());

		resolveSnapshot(
			mkSnapshot([
				mkItem({
					id: 'skill:project:proj-1:groundwork',
					kind: 'skill',
					name: 'groundwork',
					scope: { kind: 'project', project_id: 'proj-1' },
				}),
			])
		);

		await waitFor(() => expect(screen.getByText('groundwork')).toBeDefined());
		expect(screen.queryByText('com.ikenga.fallback-pkg')).toBeNull();
	});

	it('opens /ngwa/item/<id> on primary click for a snapshot row', async () => {
		m.pkgKernelStatus.mockResolvedValue(kernelStatus([]));
		m.ngwaSnapshot.mockResolvedValue(
			mkSnapshot([
				mkItem({
					id: 'skill:project:proj-1:groundwork',
					kind: 'skill',
					name: 'groundwork',
					scope: { kind: 'project', project_id: 'proj-1' },
				}),
			])
		);

		renderSection('proj-1');

		const row = await screen.findByText('groundwork');
		row.click();

		expect(addTab).toHaveBeenCalledWith('p1', {
			kind: 'route',
			path: `/ngwa/item/${encodeURIComponent('skill:project:proj-1:groundwork')}`,
		});
	});
});

describe('useNgwaProjectRowCount', () => {
	function renderCount(projectId: string) {
		return renderHook(() => useNgwaProjectRowCount(projectId), { wrapper: wrapper() });
	}

	it('equals the number of rendered rows', async () => {
		m.pkgKernelStatus.mockResolvedValue(kernelStatus([]));
		m.ngwaSnapshot.mockResolvedValue(
			mkSnapshot([
				mkItem({
					id: 'skill:project:proj-1:a',
					kind: 'skill',
					name: 'a',
					scope: { kind: 'project', project_id: 'proj-1' },
				}),
				mkItem({
					id: 'hook:project:proj-1:b',
					kind: 'hook',
					name: 'b',
					scope: { kind: 'project', project_id: 'proj-1' },
				}),
			])
		);

		const { result } = renderCount('proj-1');
		await waitFor(() => expect(result.current).toBe(2));
	});

	it('is 0 (badge hides) when the project has no Ngwa items', async () => {
		m.pkgKernelStatus.mockResolvedValue(kernelStatus([]));
		m.ngwaSnapshot.mockResolvedValue(mkSnapshot([]));

		const { result } = renderCount('proj-1');
		await waitFor(() => expect(result.current).toBe(0));
	});
});

describe('NgwaProjectSection — Disable / Uninstall failures are surfaced', () => {
	it('toasts when Disable is refused instead of swallowing it', async () => {
		while (useToastStore.getState().queue.length) dismissToast();
		m.ngwaSnapshot.mockReturnValue(pendingForever());
		m.pkgKernelStatus.mockResolvedValue(kernelStatus([{ id: 'com.ikenga.studio' }]));
		m.pkgSetEnabled.mockRejectedValue(new Error('not permitted in a browser session'));

		renderSection('proj-1');
		const row = await screen.findByText('com.ikenga.studio');
		fireEvent.contextMenu(row);
		const disable = (await screen.findAllByRole('menuitem')).find((el) =>
			/disable/i.test(el.textContent ?? '')
		);
		expect(disable).toBeDefined();
		fireEvent.click(disable as HTMLElement);

		await waitFor(() =>
			expect(useToastStore.getState().queue.map((t) => t.label)).toEqual([
				'Could not disable com.ikenga.studio: not permitted in a browser session',
			])
		);
		expect(useToastStore.getState().queue[0]?.variant).toBe('error');
	});
});
