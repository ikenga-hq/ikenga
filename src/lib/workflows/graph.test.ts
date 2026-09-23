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

  // ── Round-29 review fixes ────────────────────────────────────────────────

  it('pins source_ref to the workflow ENTRY INDEX per §10, not the workflow id', () => {
    const mkWorkflow = (id: string) => ({
      id,
      title: `Workflow ${id}`,
      steps: [
        {
          id: 'first',
          title: 'First',
          handler: '/iyke/pkg/com.ikenga.studio/first',
          inputs: {},
          produces: [],
          depends_on: [],
        },
        {
          id: 'second',
          title: 'Second',
          handler: '/iyke/pkg/com.ikenga.studio/second',
          inputs: {},
          produces: [],
          depends_on: ['first'],
        },
      ],
    });

    const manifestPath = 'packages/apps/studio/manifest.json';

    // Entry 0 — default index.
    const first = fromPkgWorkflow('com.ikenga.studio', manifestPath, mkWorkflow('nightly'));
    expect(first.nodes.map((n) => n.source_ref)).toEqual([
      `${manifestPath}#/workflows/0/steps/0`,
      `${manifestPath}#/workflows/0/steps/1`,
    ]);

    // Entry 2 — the pointer follows the index, and never mentions the id.
    const third = fromPkgWorkflow('com.ikenga.studio', manifestPath, mkWorkflow('release'), 2);
    expect(third.nodes.map((n) => n.source_ref)).toEqual([
      `${manifestPath}#/workflows/2/steps/0`,
      `${manifestPath}#/workflows/2/steps/1`,
    ]);
    for (const node of third.nodes) {
      expect(node.source_ref).not.toContain('release');
    }
  });

  it('reports each root component’s own cycles across two independent components', () => {
    // Component A (nodes a1→a2→a3) is acyclic and is walked FIRST.
    // Component B (b1→b2→b3→b1) is cyclic. Before the fix the cyclic walk left
    // `currentPath` un-unwound, so the reported cycle was polluted with
    // ancestors — or a clean component was reported cyclic.
    const graph = createWorkflowGraph({
      id: 'two-components',
      title: 'Two Components',
      source: 'manual',
      nodes: [
        { id: 'a1', kind: 'step', label: 'A1', run: null },
        { id: 'a2', kind: 'step', label: 'A2', run: null },
        { id: 'a3', kind: 'step', label: 'A3', run: null },
        { id: 'b1', kind: 'step', label: 'B1', run: null },
        { id: 'b2', kind: 'step', label: 'B2', run: null },
        { id: 'b3', kind: 'step', label: 'B3', run: null },
      ],
      edges: [
        { from: 'a1', to: 'a2', kind: 'depends-on' },
        { from: 'a2', to: 'a3', kind: 'depends-on' },
        { from: 'b1', to: 'b2', kind: 'depends-on' },
        { from: 'b2', to: 'b3', kind: 'depends-on' },
        { from: 'b3', to: 'b1', kind: 'depends-on' },
      ],
    });

    const check = validateWorkflowDag(graph);
    expect(check.valid).toBe(false);
    // Exactly one cycle, entirely inside component B.
    expect(check.cycles?.length).toBe(1);
    expect(check.cycles?.[0]).toEqual(['b1', 'b2', 'b3', 'b1']);
    // No node of the acyclic component leaked into the reported cycle.
    for (const id of check.cycles![0]) {
      expect(id.startsWith('a')).toBe(false);
    }
  });

  it('does not report a cycle for an acyclic component walked after a cyclic one', () => {
    // Cyclic component FIRST this time, so a leaked `visiting` mark would make
    // the clean component look cyclic.
    const graph = createWorkflowGraph({
      id: 'cyclic-then-clean',
      title: 'Cyclic then clean',
      source: 'manual',
      nodes: [
        { id: 'c1', kind: 'step', label: 'C1', run: null },
        { id: 'c2', kind: 'step', label: 'C2', run: null },
        { id: 'd1', kind: 'step', label: 'D1', run: null },
        { id: 'd2', kind: 'step', label: 'D2', run: null },
      ],
      edges: [
        { from: 'c1', to: 'c2', kind: 'depends-on' },
        { from: 'c2', to: 'c1', kind: 'depends-on' },
        { from: 'd1', to: 'd2', kind: 'depends-on' },
      ],
    });

    const check = validateWorkflowDag(graph);
    expect(check.valid).toBe(false);
    expect(check.cycles?.length).toBe(1);
    expect(check.cycles?.[0]).toEqual(['c1', 'c2', 'c1']);
  });

  it('reports one cycle per independent cyclic component', () => {
    const graph = createWorkflowGraph({
      id: 'two-cycles',
      title: 'Two cycles',
      source: 'manual',
      nodes: [
        { id: 'x1', kind: 'step', label: 'X1', run: null },
        { id: 'x2', kind: 'step', label: 'X2', run: null },
        { id: 'y1', kind: 'step', label: 'Y1', run: null },
        { id: 'y2', kind: 'step', label: 'Y2', run: null },
      ],
      edges: [
        { from: 'x1', to: 'x2', kind: 'depends-on' },
        { from: 'x2', to: 'x1', kind: 'depends-on' },
        { from: 'y1', to: 'y2', kind: 'depends-on' },
        { from: 'y2', to: 'y1', kind: 'depends-on' },
      ],
    });

    const check = validateWorkflowDag(graph);
    expect(check.valid).toBe(false);
    expect(check.cycles).toEqual([
      ['x1', 'x2', 'x1'],
      ['y1', 'y2', 'y1'],
    ]);
  });
});
