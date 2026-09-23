import { render, screen, cleanup, waitFor } from '@testing-library/react';
import { describe, expect, it, afterEach, vi } from 'vitest';
import { AutomationsSection, automationsContextMenu, listDeclaredWorkflows } from './automations';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import * as tauriCmd from '@/lib/tauri-cmd';

vi.mock('@/lib/tauri-cmd', () => ({
  pkgKernelStatus: vi.fn(),
  pkgPreviewManifest: vi.fn(),
}));

vi.mock('@/lib/panes/pane-store', () => ({
  usePaneStore: { getState: () => ({ focusedId: 'p1', addTab: vi.fn() }) },
}));

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

function renderSection() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <AutomationsSection projectId="test-project" />
    </QueryClientProvider>,
  );
}

function kernelStatus(pkgs: Array<{ id: string; install_path: string; enabled?: boolean }>) {
  return {
    api_version: 5,
    registries: {},
    installed: pkgs.map((p) => ({
      id: p.id,
      version: '1.0.0',
      ikenga_api: '5',
      install_path: p.install_path,
      enabled: p.enabled ?? true,
      installed_at: 0,
      compatible: true,
      source: { kind: 'local' },
    })),
  } as never;
}

const STUDIO_MANIFEST = {
  id: 'com.ikenga.studio',
  name: 'Studio',
  version: '1.0.0',
  ikenga_api: '5',
  workflows: [
    {
      id: 'nightly',
      title: 'Nightly Build',
      steps: [
        {
          id: 'build',
          title: 'Build',
          handler: '/iyke/pkg/com.ikenga.studio/build',
          inputs: {},
          produces: [],
          depends_on: [],
        },
        {
          id: 'publish',
          title: 'Publish',
          handler: '/iyke/pkg/com.ikenga.studio/publish',
          inputs: {},
          produces: [],
          depends_on: ['build'],
        },
      ],
    },
  ],
} as never;

describe('AutomationsSection (WP-04 contract / WP-31)', () => {
  it('exports section context menu following WP-04 contract', () => {
    expect(automationsContextMenu).toBeDefined();
    expect(automationsContextMenu.length).toBeGreaterThanOrEqual(4);
    expect(automationsContextMenu.map((m) => m.id)).toContain('run-now');
    expect(automationsContextMenu.map((m) => m.id)).toContain('open-definition');
  });

  it('renders empty state when no pkg declares workflows[]', async () => {
    vi.mocked(tauriCmd.pkgKernelStatus).mockResolvedValue(
      kernelStatus([{ id: 'com.ikenga.hello', install_path: '/pkgs/hello' }]),
    );
    vi.mocked(tauriCmd.pkgPreviewManifest).mockResolvedValue({
      id: 'com.ikenga.hello',
      name: 'Hello',
      version: '1.0.0',
      ikenga_api: '5',
    } as never);

    renderSection();

    await waitFor(() => expect(screen.getByText('Nothing scheduled')).toBeDefined());
    expect(screen.getByText('New schedule')).toBeDefined();
  });

  it('lists workflows[] entries across installed pkgs from the kernel snapshot', async () => {
    vi.mocked(tauriCmd.pkgKernelStatus).mockResolvedValue(
      kernelStatus([
        { id: 'com.ikenga.studio', install_path: '/pkgs/studio' },
        { id: 'com.ikenga.hello', install_path: '/pkgs/hello' },
      ]),
    );
    vi.mocked(tauriCmd.pkgPreviewManifest).mockImplementation(async (installPath: string) =>
      installPath === '/pkgs/studio'
        ? STUDIO_MANIFEST
        : ({ id: 'com.ikenga.hello', name: 'Hello', version: '1.0.0', ikenga_api: '5' } as never),
    );

    renderSection();

    await waitFor(() => expect(screen.getByText('Nightly Build')).toBeDefined());
    // Step count stands in for a schedule expression for a declared workflow.
    expect(screen.getByText('2 steps')).toBeDefined();
    expect(screen.queryByText('Nothing scheduled')).toBeNull();
  });

  it('skips disabled pkgs and survives an unreadable manifest', async () => {
    vi.mocked(tauriCmd.pkgKernelStatus).mockResolvedValue(
      kernelStatus([
        { id: 'com.ikenga.studio', install_path: '/pkgs/studio' },
        { id: 'com.ikenga.broken', install_path: '/pkgs/broken' },
        { id: 'com.ikenga.off', install_path: '/pkgs/off', enabled: false },
      ]),
    );
    vi.mocked(tauriCmd.pkgPreviewManifest).mockImplementation(async (installPath: string) => {
      if (installPath === '/pkgs/studio') return STUDIO_MANIFEST;
      if (installPath === '/pkgs/broken') throw new Error('manifest unreadable');
      throw new Error('disabled pkg must not be read');
    });

    const items = await listDeclaredWorkflows();

    expect(items.map((i) => i.id)).toEqual(['com.ikenga.studio:nightly']);
    expect(items[0].kind).toBe('workflow');
    expect(tauriCmd.pkgPreviewManifest).not.toHaveBeenCalledWith('/pkgs/off');
  });
});
