import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import { describe, expect, it, afterEach, vi } from 'vitest';
import { NgwaFlowRenderer } from './ngwa-flow-renderer';
import { createWorkflowGraph } from '@/lib/workflows/graph';

afterEach(() => {
  cleanup();
});

describe('NgwaFlowRenderer (WP-31 / flow analyze surface)', () => {
  it('renders an empty state when graph has no nodes', () => {
    const graph = createWorkflowGraph({
      id: 'empty-wf',
      title: 'Empty Workflow',
      source: 'manual',
      nodes: [],
      edges: [],
    });

    render(<NgwaFlowRenderer graph={graph} />);
    expect(screen.getByText('Empty Workflow Graph')).toBeDefined();
    expect(screen.getByText('No steps or phases declared in this workflow.')).toBeDefined();
  });

  it('renders sample workflow with phases, steps, and edges', () => {
    const graph = createWorkflowGraph({
      id: 'build-pipeline',
      title: 'Production Build Pipeline',
      source: 'groundwork',
      sourcePath: 'plans/build.md',
      nodes: [
        {
          id: 'wave-1',
          kind: 'phase',
          label: 'Phase 1: Build',
          run: null,
        },
        {
          id: 'step-compile',
          kind: 'step',
          label: 'Compile TypeScript',
          run: { kind: 'command', ref: 'tsc --noEmit' },
        },
        {
          id: 'step-bundle',
          kind: 'step',
          label: 'Bundle Assets',
          run: { kind: 'command', ref: 'vite build' },
        },
      ],
      edges: [
        { from: 'wave-1', to: 'step-compile', kind: 'depends-on' },
        { from: 'step-compile', to: 'step-bundle', kind: 'depends-on' },
      ],
    });

    const onSelectNode = vi.fn();
    const onRunNode = vi.fn();

    render(
      <NgwaFlowRenderer
        graph={graph}
        activeNodeId="step-compile"
        onSelectNode={onSelectNode}
        onRunNode={onRunNode}
      />,
    );

    // Title and source
    expect(screen.getByText('Production Build Pipeline')).toBeDefined();
    expect(screen.getByText('groundwork')).toBeDefined();
    expect(screen.getByText('plans/build.md')).toBeDefined();

    // Node labels
    expect(screen.getByText('Phase 1: Build')).toBeDefined();
    expect(screen.getByText('Compile TypeScript')).toBeDefined();
    expect(screen.getByText('Bundle Assets')).toBeDefined();

    // Command refs
    expect(screen.getByText('tsc --noEmit')).toBeDefined();
    expect(screen.getByText('vite build')).toBeDefined();

    // Node click triggers onSelectNode
    fireEvent.click(screen.getByText('Bundle Assets'));
    expect(onSelectNode).toHaveBeenCalledWith('step-bundle');

    // Run button triggers onRunNode
    const runButtons = screen.getAllByTitle('Run step');
    expect(runButtons.length).toBeGreaterThan(0);
    fireEvent.click(runButtons[0]);
    expect(onRunNode).toHaveBeenCalled();
  });
});
