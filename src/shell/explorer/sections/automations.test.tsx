import { render, screen, cleanup } from '@testing-library/react';
import { describe, expect, it, afterEach } from 'vitest';
import { AutomationsSection, automationsContextMenu } from './automations';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

afterEach(() => {
  cleanup();
});

describe('AutomationsSection (WP-04 contract / WP-31)', () => {
  it('exports section context menu following WP-04 contract', () => {
    expect(automationsContextMenu).toBeDefined();
    expect(automationsContextMenu.length).toBeGreaterThanOrEqual(4);
    expect(automationsContextMenu.map((m) => m.id)).toContain('run-now');
    expect(automationsContextMenu.map((m) => m.id)).toContain('open-definition');
  });

  it('renders empty state when no automations are scheduled', () => {
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });

    render(
      <QueryClientProvider client={queryClient}>
        <AutomationsSection projectId="test-project" />
      </QueryClientProvider>,
    );

    expect(screen.getByText('Nothing scheduled')).toBeDefined();
    expect(screen.getByText('New schedule')).toBeDefined();
  });
});
