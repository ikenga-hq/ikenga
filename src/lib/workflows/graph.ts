import {
  type WorkflowEdge,
  type WorkflowEdgeKind,
  type WorkflowGraph,
  type WorkflowNode,
  type WorkflowNodeKind,
  type WorkflowStep,
  type WorkflowEntry,
  type NgwaScope,
  WorkflowEdgeSchema,
  WorkflowGraphSchema,
  WorkflowNodeSchema,
  WORKFLOW_EDGE_KINDS,
  WORKFLOW_NODE_KINDS,
} from '@ikenga/contract';

export {
  type WorkflowEdge,
  type WorkflowEdgeKind,
  type WorkflowGraph,
  type WorkflowNode,
  type WorkflowNodeKind,
  type WorkflowStep,
  type WorkflowEntry,
  type NgwaScope,
  WorkflowEdgeSchema,
  WorkflowGraphSchema,
  WorkflowNodeSchema,
  WORKFLOW_EDGE_KINDS,
  WORKFLOW_NODE_KINDS,
};

export interface CreateWorkflowGraphOptions {
  id: string;
  title: string;
  scope?: NgwaScope;
  source: 'groundwork' | 'claude-workflow' | 'plugin' | 'hooks' | 'cron' | 'agent-ops' | 'manual';
  sourcePath?: string | null;
  nodes?: WorkflowNode[];
  edges?: WorkflowEdge[];
  updatedAtMs?: number | null;
}

/**
 * Construct and validate a WorkflowGraph object.
 */
export function createWorkflowGraph(options: CreateWorkflowGraphOptions): WorkflowGraph {
  return WorkflowGraphSchema.parse({
    id: options.id,
    title: options.title,
    scope: options.scope ?? { kind: 'personal' },
    source: options.source,
    source_path: options.sourcePath ?? null,
    nodes: options.nodes ?? [],
    edges: options.edges ?? [],
    updated_at_ms: options.updatedAtMs ?? Date.now(),
  });
}

/**
 * Map a package manifest's workflow entry to a WorkflowGraph per G-MANIFEST-V5 §10 mapping rule:
 * - id = `${pkg_id}:${workflow.id}`
 * - title = workflow.title
 * - source_path = pkgManifestPath
 * - scope = { kind: 'personal' }
 * - source = 'plugin'
 * - Each step becomes a `step` node with `run: { kind: 'command', ref: step.handler }`
 * - Each depends_on entry becomes a `depends-on` edge
 */
export function fromPkgWorkflow(
  pkgId: string,
  pkgManifestPath: string,
  workflow: WorkflowEntry,
): WorkflowGraph {
  const nodes: WorkflowNode[] = workflow.steps.map((step, idx) => ({
    id: step.id,
    kind: 'step',
    label: step.title,
    run: {
      kind: 'command',
      ref: step.handler,
    },
    source_ref: `${pkgManifestPath}#/workflows/${workflow.id}/steps/${idx}`,
  }));

  const edges: WorkflowEdge[] = [];
  for (const step of workflow.steps) {
    for (const depId of step.depends_on) {
      edges.push({
        from: depId,
        to: step.id,
        kind: 'depends-on',
      });
    }
  }

  return createWorkflowGraph({
    id: `${pkgId}:${workflow.id}`,
    title: workflow.title,
    scope: { kind: 'personal' },
    source: 'plugin',
    sourcePath: pkgManifestPath,
    nodes,
    edges,
  });
}

/**
 * Detect cycles in a workflow graph DAG.
 * Returns valid: true if acyclic, or valid: false with detected cycle node IDs.
 */
export function validateWorkflowDag(graph: WorkflowGraph): { valid: boolean; cycles?: string[][] } {
  const adj = new Map<string, string[]>();
  for (const node of graph.nodes) {
    adj.set(node.id, []);
  }
  for (const edge of graph.edges) {
    if (edge.kind === 'depends-on') {
      const list = adj.get(edge.from);
      if (list) {
        list.push(edge.to);
      }
    }
  }

  const visited = new Map<string, 'unvisited' | 'visiting' | 'visited'>();
  for (const node of graph.nodes) {
    visited.set(node.id, 'unvisited');
  }

  const cycles: string[][] = [];
  const currentPath: string[] = [];

  function dfs(nodeId: string): boolean {
    visited.set(nodeId, 'visiting');
    currentPath.push(nodeId);

    const neighbors = adj.get(nodeId) ?? [];
    for (const neighbor of neighbors) {
      const state = visited.get(neighbor);
      if (state === 'visiting') {
        const cycleStartIndex = currentPath.indexOf(neighbor);
        cycles.push([...currentPath.slice(cycleStartIndex), neighbor]);
        return false;
      }
      if (state === 'unvisited') {
        if (!dfs(neighbor)) {
          return false;
        }
      }
    }

    currentPath.pop();
    visited.set(nodeId, 'visited');
    return true;
  }

  for (const node of graph.nodes) {
    if (visited.get(node.id) === 'unvisited') {
      dfs(node.id);
    }
  }

  return {
    valid: cycles.length === 0,
    cycles: cycles.length > 0 ? cycles : undefined,
  };
}

/**
 * Returns topological sort of node IDs based on `depends-on` edges.
 */
export function getTopologicalOrder(graph: WorkflowGraph): string[] {
  const inDegree = new Map<string, number>();
  const adj = new Map<string, string[]>();

  for (const node of graph.nodes) {
    inDegree.set(node.id, 0);
    adj.set(node.id, []);
  }

  for (const edge of graph.edges) {
    if (edge.kind === 'depends-on') {
      const targets = adj.get(edge.from);
      if (targets) {
        targets.push(edge.to);
      }
      inDegree.set(edge.to, (inDegree.get(edge.to) ?? 0) + 1);
    }
  }

  const queue: string[] = [];
  for (const [id, deg] of inDegree.entries()) {
    if (deg === 0) {
      queue.push(id);
    }
  }

  const order: string[] = [];
  while (queue.length > 0) {
    const curr = queue.shift()!;
    order.push(curr);
    for (const nxt of adj.get(curr) ?? []) {
      const deg = (inDegree.get(nxt) ?? 1) - 1;
      inDegree.set(nxt, deg);
      if (deg === 0) {
        queue.push(nxt);
      }
    }
  }

  return order;
}

/**
 * Find incoming edges for a given node.
 */
export function getNodeIncomingEdges(graph: WorkflowGraph, nodeId: string): WorkflowEdge[] {
  return graph.edges.filter((e) => e.to === nodeId);
}

/**
 * Find outgoing edges from a given node.
 */
export function getNodeOutgoingEdges(graph: WorkflowGraph, nodeId: string): WorkflowEdge[] {
  return graph.edges.filter((e) => e.from === nodeId);
}
