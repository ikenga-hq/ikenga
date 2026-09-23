import { describe, it, expect } from 'vitest';
import {
  createWorkflowGraph,
  fromPkgWorkflow,
  validateWorkflowDag,
  getTopologicalOrder,
} from './graph';

describe('WorkflowGraph adapter', () => {
  it('constructs a valid WorkflowGraph and validates DAG', () => {
    const graph = createWorkflowGraph({
      id: 'test-graph',
      title: 'Test Workflow',
      source: 'manual',
      nodes: [
        { id: 'step-1', kind: 'step', label: 'Step 1', run: { kind: 'command', ref: 'echo 1' } },
        { id: 'step-2', kind: 'step', label: 'Step 2', run: { kind: 'command', ref: 'echo 2' } },
      ],
      edges: [
        { from: 'step-1', to: 'step-2', kind: 'depends-on' },
      ],
    });

    expect(graph.id).toBe('test-graph');
    expect(graph.nodes.length).toBe(2);
    expect(graph.edges.length).toBe(1);

    const check = validateWorkflowDag(graph);
    expect(check.valid).toBe(true);

    const order = getTopologicalOrder(graph);
    expect(order).toEqual(['step-1', 'step-2']);
  });

  it('detects cycles in graph', () => {
    const cyclicGraph = createWorkflowGraph({
      id: 'cyclic-graph',
      title: 'Cyclic Workflow',
      source: 'manual',
      nodes: [
        { id: 'a', kind: 'step', label: 'A', run: null },
        { id: 'b', kind: 'step', label: 'B', run: null },
      ],
      edges: [
        { from: 'a', to: 'b', kind: 'depends-on' },
        { from: 'b', to: 'a', kind: 'depends-on' },
      ],
    });

    const check = validateWorkflowDag(cyclicGraph);
    expect(check.valid).toBe(false);
    expect(check.cycles?.length).toBeGreaterThan(0);
  });

  it('maps package workflow to WorkflowGraph per DEC-41 / §10 mapping rule', () => {
    const wf = {
      id: 'build-and-deploy',
      title: 'Build & Deploy',
      steps: [
        {
          id: 'build',
          title: 'Build Step',
          handler: '/iyke/pkg/com.ikenga.studio/build',
          inputs: {},
          produces: ['bundle.js'],
          depends_on: [],
        },
        {
          id: 'deploy',
          title: 'Deploy Step',
          handler: '/iyke/pkg/com.ikenga.studio/deploy',
          inputs: {},
          produces: [],
          depends_on: ['build'],
        },
      ],
    };

    const graph = fromPkgWorkflow('com.ikenga.studio', 'packages/apps/studio/manifest.json', wf);

    expect(graph.id).toBe('com.ikenga.studio:build-and-deploy');
    expect(graph.title).toBe('Build & Deploy');
    expect(graph.source).toBe('plugin');
    expect(graph.scope).toEqual({ kind: 'personal' });
    expect(graph.source_path).toBe('packages/apps/studio/manifest.json');

    expect(graph.nodes.length).toBe(2);
    expect(graph.nodes[0].id).toBe('build');
    expect(graph.nodes[0].run).toEqual({
      kind: 'command',
      ref: '/iyke/pkg/com.ikenga.studio/build',
    });

    expect(graph.edges.length).toBe(1);
    expect(graph.edges[0]).toEqual({
      from: 'build',
      to: 'deploy',
      kind: 'depends-on',
    });
  });
});
