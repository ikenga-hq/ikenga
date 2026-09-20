import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';

import { FilesSection } from './files';
import { useShellStore } from '@/lib/shell/shell-store';
import { useFilesStore } from '@/lib/shell/files-store';
import { useGitStatus } from '@/lib/shell/use-git-status';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

vi.mock('@/lib/shell/shell-store', () => ({
  useShellStore: vi.fn()
}));
vi.mock('@/lib/shell/files-store', () => ({
  useFilesStore: vi.fn()
}));
vi.mock('@/lib/shell/use-git-status', () => ({
  useGitStatus: vi.fn()
}));
vi.mock('@/lib/tauri-cmd', () => ({
  fsList: vi.fn().mockResolvedValue([
    { name: 'src', path: '/test-root/src', isDir: true },
    { name: 'package.json', path: '/test-root/package.json', isDir: false }
  ]),
  fsSearch: vi.fn(),
  fsRename: vi.fn(),
  fsTrash: vi.fn(),
}));

// Mock ResizeObserver for radix components
global.ResizeObserver = class ResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
};

const queryClient = new QueryClient({
  defaultOptions: { queries: { retry: false } }
});

describe('FilesMode Characterisation', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('renders files mode and lists root', async () => {
    (useShellStore as any).mockImplementation((selector: any) => {
      const state = {
        activeProject: { id: 'proj-1', root_path: '/test-root', extra_roots: [] },
        activeProjectId: 'proj-1',
      };
      return selector(state);
    });
    
    (useFilesStore as any).mockImplementation((selector: any) => {
      const state = {
        hydrated: true,
        hydrate: vi.fn(),
        expandedRoot: '/test-root',
        setExpandedRoot: vi.fn(),
        expanded: new Set(),
        queries: {},
        setQuery: vi.fn(),
        toggleRoot: vi.fn(),
        showHidden: false,
        showIgnored: false,
        scrollTop: 0,
        setScrollTop: vi.fn(),
        reveal: vi.fn(),
        toggleShowHidden: vi.fn()
      };
      return selector(state);
    });
    
    (useGitStatus as any).mockReturnValue({
      data: {
        files: new Map([['/test-root/package.json', 'modified']]),
        dirtyFolders: new Set(['/test-root/src'])
      }
    } as any);

    render(
      <QueryClientProvider client={queryClient}>
        <FilesSection projectId="proj-1" />
      </QueryClientProvider>
    );

    expect(screen.getByText('/test-root')).toBeTruthy();
    
    await waitFor(() => {
      expect(screen.getByText('src')).toBeTruthy();
      expect(screen.getByText('package.json')).toBeTruthy();
    });
  });
});
