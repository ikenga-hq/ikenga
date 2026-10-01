import { render, screen, cleanup, waitFor } from '@testing-library/react';
import { describe, expect, it, afterEach, vi } from 'vitest';
import { NgwaProjectSection } from './ngwa-project';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import * as tauriCmd from '@/lib/tauri-cmd';

const addTab = vi.fn();

vi.mock('@/lib/tauri-cmd', () => ({
	pkgKernelStatus: vi.fn(),
	pkgSetEnabled: vi.fn(),
	pkgUninstall: vi.fn(),
}));

vi.mock('@/lib/panes/pane-store', () => ({
	usePaneStore: { getState: () => ({ focusedId: 'p1', addTab }) },
}));

afterEach(() => {
	cleanup();
	vi.resetAllMocks();
});

function renderSection(projectId: string) {
	const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return render(
		<QueryClientProvider client={queryClient}>
			<NgwaProjectSection projectId={projectId} />
		</QueryClientProvider>
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

describe('NgwaProjectSection — primary row click (fix/explorer-ngwa-open-detail)', () => {
	it.each([['com.ikenga.engine-claude-code'], ['com.ikenga.iyke-skill'], ['com.ikenga.studio']])(
		'opens the Ngwa item detail route for %s, not /pkg/<id>',
		async (pkgId) => {
			vi.mocked(tauriCmd.pkgKernelStatus).mockResolvedValue(kernelStatus([{ id: pkgId }]));

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
		vi.mocked(tauriCmd.pkgKernelStatus).mockResolvedValue(
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
