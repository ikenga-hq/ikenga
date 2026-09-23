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
 * - `source_ref` = `<manifest>#/workflows/<entry index>/steps/<step index>`
 * - Each depends_on entry becomes a `depends-on` edge
 */
export function fromPkgWorkflow(
  pkgId: string,
  pkgManifestPath: string,
  workflow: WorkflowEntry,
  /**
   * The entry's index in `manifest.workflows[]`. §10 pins `source_ref` to
   * `<manifest>#/workflows/<i>/steps/<j>` — a **JSON Pointer into the
   * manifest**, so `<i>` is the entry index, never the workflow id (which is
   * not addressable by pointer). Defaults to 0 for a single-entry manifest.
   */
  workflowIndex = 0,
): WorkflowGraph {
  const nodes: WorkflowNode[] = workflow.steps.map((step, idx) => ({
    id: step.id,
    kind: 'step',
    label: step.title,
    run: {
      kind: 'command',
      ref: step.handler,
    },
    source_ref: `${pkgManifestPath}#/workflows/${workflowIndex}/steps/${idx}`,
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
 *
 * Each connected component is walked independently: the DFS always unwinds
 * `currentPath` and settles every node it entered to `visited`, including on
 * the path that records a cycle. (Before the WP-31 Round-29 fix the early
 * `return false` left the offending path on `currentPath` and its nodes stuck
 * in `visiting`, so a *later* root spliced stale ancestors into its own
 * `cycles[]` entry — or reported a cycle where there was none.)
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

  /** Returns true when a cycle was recorded anywhere below `nodeId`. */
  function dfs(nodeId: string): boolean {
    visited.set(nodeId, 'visiting');
    currentPath.push(nodeId);

    let foundCycle = false;
    for (const neighbor of adj.get(nodeId) ?? []) {
      const state = visited.get(neighbor);
      if (state === 'visiting') {
        // Back edge — the cycle is the `currentPath` suffix from `neighbor`.
        const cycleStartIndex = currentPath.indexOf(neighbor);
        cycles.push([...currentPath.slice(cycleStartIndex), neighbor]);
        foundCycle = true;
        // Keep scanning siblings; do NOT bail out — bailing is what left the
        // path and the `visiting` marks un-unwound.
        continue;
      }
      if (state === 'unvisited' && dfs(neighbor)) {
        foundCycle = true;
      }
    }

    // Unwind unconditionally so a later root starts from a clean path.
    currentPath.pop();
    visited.set(nodeId, 'visited');
    return foundCycle;
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
